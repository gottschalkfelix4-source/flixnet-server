//! The server log in memory: a `tracing` layer keeps the last [`CAPACITY`] lines in a
//! ring buffer, which administrators read (and tail by polling) via `/api/v1/logs`.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{LazyLock, Mutex};

use axum::Json;
use axum::extract::Query;
use serde::{Deserialize, Serialize};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::AdminUser;
use crate::error::{ApiResult, ErrorBody};

/// Lines kept in memory.
pub const CAPACITY: usize = 2000;

/// Severity, most severe first (so `level <= filter` keeps what a filter asks for).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl From<&Level> for LogLevel {
    fn from(level: &Level) -> Self {
        match *level {
            Level::ERROR => LogLevel::Error,
            Level::WARN => LogLevel::Warn,
            Level::INFO => LogLevel::Info,
            Level::DEBUG => LogLevel::Debug,
            _ => LogLevel::Trace,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct LogLine {
    /// Increasing sequence number; pass the last one as `after` to tail the log.
    pub seq: u64,
    /// Unix milliseconds.
    pub timestamp: i64,
    pub level: LogLevel,
    /// Module that logged (e.g. `flixnet_library::scan`).
    pub target: String,
    /// Message plus structured fields (`key=value`).
    pub message: String,
}

/// Ring buffer of recent log lines.
pub struct LogBuffer {
    inner: Mutex<(u64, VecDeque<LogLine>)>,
    capacity: usize,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new((0, VecDeque::with_capacity(capacity))),
            capacity,
        }
    }

    pub fn push(&self, level: LogLevel, target: &str, message: String) {
        let mut guard = self.inner.lock().expect("log buffer lock");
        let (seq, lines) = &mut *guard;
        *seq += 1;
        if lines.len() == self.capacity {
            lines.pop_front();
        }
        lines.push_back(LogLine {
            seq: *seq,
            timestamp: flixnet_db::now_ms(),
            level,
            target: target.to_owned(),
            message,
        });
    }

    /// Lines after `after` at `level` or more severe; the most recent `limit` of them,
    /// oldest first. Also returns the newest sequence number overall.
    pub fn since(&self, after: u64, level: LogLevel, limit: usize) -> (Vec<LogLine>, u64) {
        let guard = self.inner.lock().expect("log buffer lock");
        let (seq, lines) = &*guard;
        let mut out: Vec<LogLine> = lines
            .iter()
            .rev()
            .filter(|l| l.seq > after && l.level <= level)
            .take(limit)
            .cloned()
            .collect();
        out.reverse();
        (out, *seq)
    }
}

/// The process-wide buffer the [`layer`] writes to.
pub static LOGS: LazyLock<LogBuffer> = LazyLock::new(|| LogBuffer::new(CAPACITY));

/// `tracing` layer feeding [`LOGS`]; add it next to the formatter.
pub fn layer() -> LogLayer {
    LogLayer
}

pub struct LogLayer;

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = Visitor::default();
        event.record(&mut visitor);
        let metadata = event.metadata();
        LOGS.push(
            LogLevel::from(metadata.level()),
            metadata.target(),
            visitor.finish(),
        );
    }
}

#[derive(Default)]
struct Visitor {
    message: String,
    fields: String,
}

impl Visitor {
    fn finish(self) -> String {
        if self.fields.is_empty() {
            self.message
        } else if self.message.is_empty() {
            self.fields.trim_start().to_owned()
        } else {
            format!("{}{}", self.message, self.fields)
        }
    }
}

impl Visit for Visitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message.push_str(value),
            name if name.starts_with("log.") => {}
            name => {
                let _ = write!(self.fields, " {name}={value}");
            }
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => {
                let _ = write!(self.message, "{value:?}");
            }
            name if name.starts_with("log.") => {}
            name => {
                let _ = write!(self.fields, " {name}={value:?}");
            }
        }
    }
}

pub(crate) fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(list_logs))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LogQuery {
    /// Least severe level to include (default `info`).
    pub level: Option<LogLevel>,
    /// Only lines with a higher sequence number (for tailing).
    pub after: Option<u64>,
    /// 1–2000, default 500 (the most recent ones).
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LogsDto {
    pub lines: Vec<LogLine>,
    /// Newest sequence number; pass as `after` on the next poll.
    pub last_seq: u64,
}

/// Recent server log lines (administrators).
#[utoipa::path(
    get,
    path = "/logs",
    operation_id = "listLogs",
    tag = "dashboard",
    params(LogQuery),
    responses(
        (status = OK, description = "Log lines, oldest first", body = LogsDto),
        (status = UNAUTHORIZED, description = "Missing or invalid token", body = ErrorBody),
        (status = FORBIDDEN, description = "Not an administrator", body = ErrorBody),
    )
)]
async fn list_logs(_: AdminUser, Query(query): Query<LogQuery>) -> ApiResult<Json<LogsDto>> {
    let (lines, last_seq) = LOGS.since(
        query.after.unwrap_or(0),
        query.level.unwrap_or_default(),
        query.limit.unwrap_or(500).clamp(1, CAPACITY),
    );
    Ok(Json(LogsDto { lines, last_seq }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn keeps_the_most_recent_lines_and_filters_by_level() {
        let buffer = LogBuffer::new(3);
        for n in 0..5 {
            buffer.push(LogLevel::Info, "t", format!("info {n}"));
        }
        buffer.push(LogLevel::Warn, "t", "warn".into());
        let (all, last) = buffer.since(0, LogLevel::Trace, 10);
        assert_eq!(last, 6);
        assert_eq!(
            all.iter().map(|l| l.message.as_str()).collect::<Vec<_>>(),
            ["info 3", "info 4", "warn"]
        );
        let (warn, _) = buffer.since(0, LogLevel::Warn, 10);
        assert_eq!(warn.len(), 1);
        let (tail, _) = buffer.since(5, LogLevel::Trace, 10);
        assert_eq!(tail.len(), 1);
        let (limited, _) = buffer.since(0, LogLevel::Trace, 2);
        assert_eq!(limited[0].message, "info 4");
    }

    #[test]
    fn the_layer_records_messages_and_fields() {
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                library = "Filme",
                count = 3,
                "scan finished unique-marker-17"
            );
        });
        let (lines, _) = LOGS.since(0, LogLevel::Warn, CAPACITY);
        let line = lines
            .iter()
            .find(|l| l.message.contains("unique-marker-17"))
            .expect("line recorded");
        assert_eq!(line.level, LogLevel::Warn);
        assert!(line.message.contains("library=Filme"), "{}", line.message);
        assert!(line.message.contains("count=3"), "{}", line.message);
    }
}

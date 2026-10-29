//! Recurring tasks (rescans, cleanup, EPG refresh) that enqueue jobs on an interval.
//! Intervals and on/off state live in the database so admins can change them.

use std::sync::Arc;
use std::time::Duration;

use flixnet_db::{Db, DbError, jobs};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::jobs::{JobHandler, JobOptions, JobQueue, Pool, priority};

/// A named recurring task that enqueues one job kind with a fixed payload.
#[derive(Debug, Clone)]
pub struct TaskDef {
    pub name: &'static str,
    pub description: &'static str,
    pub default_interval: Duration,
    kind: &'static str,
    pool: Pool,
    max_attempts: i64,
    payload: String,
}

impl TaskDef {
    pub fn new<H: JobHandler>(
        name: &'static str,
        description: &'static str,
        default_interval: Duration,
        payload: &H::Payload,
    ) -> Self {
        Self {
            name,
            description,
            default_interval,
            kind: H::KIND,
            pool: H::POOL,
            max_attempts: H::MAX_ATTEMPTS,
            payload: serde_json::to_string(payload).expect("task payload serializes"),
        }
    }

    /// Enqueues the task's job (deduplicated: a task never runs twice at once).
    pub async fn enqueue(&self, queue: &JobQueue) -> Result<Option<i64>, DbError> {
        queue
            .enqueue_raw(
                self.kind,
                self.pool,
                &self.payload,
                self.max_attempts,
                JobOptions::priority(priority::BACKFILL).dedupe(format!("task:{}", self.name)),
            )
            .await
    }
}

/// Registered tasks, shared with the API ("run now", list).
#[derive(Debug, Clone, Default)]
pub struct TaskRegistry {
    tasks: Arc<Vec<TaskDef>>,
}

impl TaskRegistry {
    pub fn new(tasks: Vec<TaskDef>) -> Self {
        Self {
            tasks: Arc::new(tasks),
        }
    }

    pub fn get(&self, name: &str) -> Option<&TaskDef> {
        self.tasks.iter().find(|t| t.name == name)
    }

    pub fn all(&self) -> &[TaskDef] {
        &self.tasks
    }

    /// Runs a task now and records it as run.
    pub async fn run_now(&self, db: &Db, queue: &JobQueue, name: &str) -> Result<bool, DbError> {
        let Some(task) = self.get(name) else {
            return Ok(false);
        };
        task.enqueue(queue).await?;
        jobs::mark_task_run(db, name, flixnet_db::now_ms()).await?;
        Ok(true)
    }

    /// Ensures every task has a row (first run one interval from now), then checks for
    /// due tasks every `tick` until cancelled.
    pub async fn start(
        self,
        db: Db,
        queue: JobQueue,
        tick: Duration,
        shutdown: CancellationToken,
    ) -> Result<JoinHandle<()>, DbError> {
        let now = flixnet_db::now_ms();
        for task in self.all() {
            let interval = task.default_interval.as_millis() as i64;
            jobs::ensure_task(&db, task.name, interval, now + interval).await?;
        }
        Ok(tokio::spawn(async move {
            loop {
                if let Err(err) = self.run_due(&db, &queue).await {
                    tracing::error!(%err, "running scheduled tasks failed");
                }
                tokio::select! {
                    () = tokio::time::sleep(tick) => {}
                    () = shutdown.cancelled() => break,
                }
            }
        }))
    }

    async fn run_due(&self, db: &Db, queue: &JobQueue) -> Result<(), DbError> {
        let now = flixnet_db::now_ms();
        for due in jobs::due_tasks(db, now).await? {
            if let Some(task) = self.get(&due.name) {
                tracing::debug!(task = task.name, "scheduled task due");
                task.enqueue(queue).await?;
            }
            jobs::mark_task_run(db, &due.name, now).await?;
        }
        Ok(())
    }
}

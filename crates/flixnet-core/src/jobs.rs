//! Persistent background jobs.
//!
//! Work (probing files, fetching metadata, thumbnails, fingerprints, embeddings) is
//! queued in SQLite and executed by per-pool dispatchers. Each pool has its own
//! concurrency so heavy CPU/GPU/AI work cannot starve quick I/O jobs. Jobs survive
//! restarts and crashes: anything left `running` is requeued when the runtime starts.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flixnet_db::{Db, DbError, jobs};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::events::{EventBus, ServerEvent};

/// Worker pools with independent concurrency limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pool {
    /// Network and database work (metadata, images).
    Io,
    /// ffprobe runs.
    Probe,
    /// CPU-heavy analysis (fingerprints, trickplay on CPU).
    Cpu,
    /// Hardware-accelerated media work.
    Gpu,
    /// Local AI models (embeddings, speech-to-text).
    Ai,
}

impl Pool {
    pub const ALL: [Pool; 5] = [Pool::Io, Pool::Probe, Pool::Cpu, Pool::Gpu, Pool::Ai];

    pub fn as_str(self) -> &'static str {
        match self {
            Pool::Io => "io",
            Pool::Probe => "probe",
            Pool::Cpu => "cpu",
            Pool::Gpu => "gpu",
            Pool::Ai => "ai",
        }
    }

    pub fn default_concurrency(self) -> usize {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        match self {
            Pool::Io => 8,
            Pool::Probe => 4,
            Pool::Cpu => (cores / 4).max(1),
            Pool::Gpu => 1,
            Pool::Ai => 1,
        }
    }
}

/// Queue priorities: higher runs first.
pub mod priority {
    /// Something a user is waiting for right now.
    pub const INTERACTIVE: i64 = 100;
    /// Newly added media.
    pub const NEW: i64 = 50;
    /// Background catch-up work.
    pub const BACKFILL: i64 = 10;
}

/// A kind of job: its name, pool and typed payload.
pub trait JobHandler: Send + Sync + 'static {
    const KIND: &'static str;
    const POOL: Pool;
    const MAX_ATTEMPTS: i64 = 3;
    type Payload: Serialize + DeserializeOwned + Send + 'static;

    fn run(
        &self,
        ctx: JobContext,
        payload: Self::Payload,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// What a running job can see and do.
#[derive(Clone)]
pub struct JobContext {
    pub id: i64,
    pub attempt: i64,
    kind: String,
    pool: Pool,
    db: Db,
    events: EventBus,
    queue: JobQueue,
    cancel: CancellationToken,
}

impl JobContext {
    /// Set when the job was cancelled or the server is shutting down.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.cancel.cancelled().await;
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Reports progress (0.0–1.0); also serves as a heartbeat.
    pub async fn progress(&self, progress: f64) {
        let progress = progress.clamp(0.0, 1.0);
        if let Err(err) = jobs::heartbeat(&self.db, self.id, Some(progress)).await {
            tracing::warn!(%err, job = self.id, "recording progress failed");
        }
        self.events.publish(ServerEvent::JobUpdated {
            id: self.id,
            kind: self.kind.clone(),
            pool: self.pool.as_str().to_owned(),
            state: "running".to_owned(),
            progress: Some(progress),
        });
    }

    /// Lets a job enqueue follow-up work.
    pub fn queue(&self) -> &JobQueue {
        &self.queue
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn events(&self) -> &EventBus {
        &self.events
    }
}

#[derive(Debug, Clone, Default)]
pub struct JobOptions {
    pub priority: i64,
    /// While a job with this key is queued or running, equal enqueues are ignored.
    pub dedupe_key: Option<String>,
    pub delay: Option<Duration>,
}

impl JobOptions {
    pub fn priority(priority: i64) -> Self {
        Self {
            priority,
            ..Default::default()
        }
    }

    pub fn dedupe(mut self, key: impl Into<String>) -> Self {
        self.dedupe_key = Some(key.into());
        self
    }
}

/// Handle for adding jobs. Cheap to clone.
#[derive(Clone)]
pub struct JobQueue {
    db: Db,
    notify: Arc<Notify>,
}

impl JobQueue {
    /// Queues a typed job. `Ok(None)` means an equal job (same dedupe key) is pending.
    pub async fn enqueue<H: JobHandler>(
        &self,
        payload: &H::Payload,
        options: JobOptions,
    ) -> Result<Option<i64>, DbError> {
        let payload = serde_json::to_string(payload)?;
        self.enqueue_raw(H::KIND, H::POOL, &payload, H::MAX_ATTEMPTS, options)
            .await
    }

    pub(crate) async fn enqueue_raw(
        &self,
        kind: &str,
        pool: Pool,
        payload: &str,
        max_attempts: i64,
        options: JobOptions,
    ) -> Result<Option<i64>, DbError> {
        let run_after = options
            .delay
            .map(|d| flixnet_db::now_ms() + d.as_millis() as i64);
        let id = jobs::enqueue(
            &self.db,
            &jobs::NewJob {
                kind,
                pool: pool.as_str(),
                payload,
                priority: options.priority,
                max_attempts,
                dedupe_key: options.dedupe_key.as_deref(),
                run_after,
            },
        )
        .await?;
        if id.is_some() {
            self.notify.notify_waiters();
        }
        Ok(id)
    }
}

type BoxFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;

/// Registered handlers by job kind.
type Handlers = HashMap<&'static str, (Pool, Arc<dyn ErasedHandler>)>;

/// Type-erased handler so different payload types live in one registry.
trait ErasedHandler: Send + Sync {
    fn run(self: Arc<Self>, ctx: JobContext, payload: &str) -> BoxFuture;
}

struct Typed<H>(H);

impl<H: JobHandler> ErasedHandler for Typed<H> {
    fn run(self: Arc<Self>, ctx: JobContext, payload: &str) -> BoxFuture {
        let payload: Result<H::Payload, _> = serde_json::from_str(payload);
        Box::pin(async move {
            let payload = payload.map_err(|e| anyhow::anyhow!("invalid payload: {e}"))?;
            self.0.run(ctx, payload).await
        })
    }
}

/// Configures handlers and pools, then starts the dispatchers.
pub struct JobRuntime {
    db: Db,
    events: EventBus,
    queue: JobQueue,
    handlers: Handlers,
    concurrency: HashMap<Pool, usize>,
    retry_backoff: Duration,
    running: Arc<Mutex<HashMap<i64, CancellationToken>>>,
}

impl JobRuntime {
    pub fn new(db: Db, events: EventBus) -> Self {
        let queue = JobQueue {
            db: db.clone(),
            notify: Arc::new(Notify::new()),
        };
        Self {
            db,
            events,
            queue,
            handlers: HashMap::new(),
            concurrency: Pool::ALL
                .iter()
                .map(|p| (*p, p.default_concurrency()))
                .collect(),
            retry_backoff: Duration::from_secs(30),
            running: Arc::default(),
        }
    }

    pub fn register<H: JobHandler>(mut self, handler: H) -> Self {
        self.handlers
            .insert(H::KIND, (H::POOL, Arc::new(Typed(handler))));
        self
    }

    pub fn concurrency(mut self, pool: Pool, workers: usize) -> Self {
        self.concurrency.insert(pool, workers.max(1));
        self
    }

    /// Base delay before a failed job is retried (doubles per attempt, capped at 1 h).
    pub fn retry_backoff(mut self, backoff: Duration) -> Self {
        self.retry_backoff = backoff;
        self
    }

    /// A handle for enqueueing and cancelling, usable before or after `start`.
    pub fn handle(&self) -> JobsHandle {
        JobsHandle {
            queue: self.queue.clone(),
            running: self.running.clone(),
            db: self.db.clone(),
        }
    }

    /// Requeues jobs a previous process left running, then starts one dispatcher per pool.
    pub async fn start(self) -> Result<RunningJobs, DbError> {
        let recovered = jobs::requeue_stale(&self.db, i64::MAX).await?;
        if recovered > 0 {
            tracing::info!(
                recovered,
                "requeued jobs interrupted by a previous shutdown"
            );
        }

        let shutdown = CancellationToken::new();
        let handlers = Arc::new(self.handlers);
        let mut dispatchers = JoinSet::new();
        for pool in Pool::ALL {
            let dispatcher = Dispatcher {
                pool,
                workers: self.concurrency.get(&pool).copied().unwrap_or(1),
                db: self.db.clone(),
                events: self.events.clone(),
                queue: self.queue.clone(),
                handlers: handlers.clone(),
                running: self.running.clone(),
                shutdown: shutdown.clone(),
                retry_backoff: self.retry_backoff,
            };
            dispatchers.spawn(dispatcher.run());
        }
        Ok(RunningJobs {
            handle: JobsHandle {
                queue: self.queue,
                running: self.running,
                db: self.db,
            },
            shutdown,
            dispatchers,
        })
    }
}

/// Enqueue and cancel jobs (shared with the API).
#[derive(Clone)]
pub struct JobsHandle {
    queue: JobQueue,
    running: Arc<Mutex<HashMap<i64, CancellationToken>>>,
    db: Db,
}

impl JobsHandle {
    pub fn queue(&self) -> &JobQueue {
        &self.queue
    }

    /// Cancels a queued or running job. Returns whether anything changed.
    pub async fn cancel(&self, id: i64) -> Result<bool, DbError> {
        let changed = jobs::cancel(&self.db, id).await?;
        if let Some(token) = self.running.lock().expect("jobs lock").get(&id) {
            token.cancel();
        }
        Ok(changed)
    }

    /// Requeues a failed or cancelled job with fresh attempts and wakes the workers.
    pub async fn retry(&self, id: i64) -> Result<bool, DbError> {
        let changed = jobs::retry(&self.db, id).await?;
        if changed {
            self.queue.notify.notify_waiters();
        }
        Ok(changed)
    }

    /// Number of jobs currently executing in this process.
    pub fn running_count(&self) -> usize {
        self.running.lock().expect("jobs lock").len()
    }
}

/// The started runtime; dropping it without `shutdown` leaves jobs to be recovered.
pub struct RunningJobs {
    handle: JobsHandle,
    shutdown: CancellationToken,
    dispatchers: JoinSet<()>,
}

impl RunningJobs {
    pub fn handle(&self) -> JobsHandle {
        self.handle.clone()
    }

    /// Stops claiming new work, signals running jobs and waits up to `grace` for them.
    /// Jobs that do not finish stay `running` and are requeued on the next start.
    pub async fn shutdown(mut self, grace: Duration) {
        self.shutdown.cancel();
        let wait_all = async { while self.dispatchers.join_next().await.is_some() {} };
        if tokio::time::timeout(grace, wait_all).await.is_err() {
            tracing::warn!("jobs did not stop in time; they will resume after restart");
            self.dispatchers.abort_all();
        }
    }

    /// Simulates a crash: kills all dispatchers and workers without bookkeeping.
    pub fn abort(mut self) {
        self.dispatchers.abort_all();
        self.dispatchers.detach_all();
    }
}

struct Dispatcher {
    pool: Pool,
    workers: usize,
    db: Db,
    events: EventBus,
    queue: JobQueue,
    handlers: Arc<Handlers>,
    running: Arc<Mutex<HashMap<i64, CancellationToken>>>,
    shutdown: CancellationToken,
    retry_backoff: Duration,
}

struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Longest a dispatcher sleeps without being notified.
const IDLE_POLL: Duration = Duration::from_secs(5);
const HEARTBEAT: Duration = Duration::from_secs(15);

impl Dispatcher {
    async fn run(self) {
        let slots = Arc::new(Semaphore::new(self.workers));
        let mut workers = JoinSet::new();
        let this = Arc::new(self);

        loop {
            let permit = tokio::select! {
                permit = slots.clone().acquire_owned() => permit.expect("semaphore open"),
                () = this.shutdown.cancelled() => break,
            };
            // Register interest *before* checking the queue so an enqueue between
            // the check and the wait cannot be missed.
            let notified = this.queue.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            match jobs::claim(&this.db, this.pool.as_str()).await {
                Ok(Some(job)) => {
                    let worker = this.clone();
                    workers.spawn(async move {
                        worker.execute(job).await;
                        drop(permit);
                    });
                }
                Ok(None) => {
                    drop(permit);
                    let wait = match jobs::next_due(&this.db, this.pool.as_str()).await {
                        Ok(Some(due)) => {
                            let ms = (due - flixnet_db::now_ms())
                                .clamp(10, IDLE_POLL.as_millis() as i64);
                            Duration::from_millis(ms as u64)
                        }
                        _ => IDLE_POLL,
                    };
                    tokio::select! {
                        () = &mut notified => {}
                        () = tokio::time::sleep(wait) => {}
                        () = this.shutdown.cancelled() => break,
                    }
                }
                Err(err) => {
                    drop(permit);
                    tracing::error!(%err, pool = this.pool.as_str(), "claiming jobs failed");
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(2)) => {}
                        () = this.shutdown.cancelled() => break,
                    }
                }
            }
        }
        // Let running workers observe the cancellation and finish.
        while workers.join_next().await.is_some() {}
    }

    async fn execute(&self, job: jobs::Job) {
        let Some((_, handler)) = self.handlers.get(job.kind.as_str()).cloned() else {
            tracing::error!(kind = %job.kind, id = job.id, "no handler registered for job kind");
            let _ = jobs::fail(&self.db, job.id, "no handler for this job kind", i64::MAX).await;
            return;
        };

        let cancel = self.shutdown.child_token();
        self.running
            .lock()
            .expect("jobs lock")
            .insert(job.id, cancel.clone());
        self.publish(&job, "running", None);

        let ctx = JobContext {
            id: job.id,
            attempt: job.attempts,
            kind: job.kind.clone(),
            pool: self.pool,
            db: self.db.clone(),
            events: self.events.clone(),
            queue: self.queue.clone(),
            cancel: cancel.clone(),
        };

        // Run in its own task so a panic becomes an error instead of killing the worker;
        // the guard aborts it if this worker itself is torn down.
        let mut task = tokio::spawn(handler.run(ctx, &job.payload));
        let _abort_guard = AbortOnDrop(task.abort_handle());
        let heartbeat = async {
            let mut tick = tokio::time::interval(HEARTBEAT);
            tick.tick().await;
            loop {
                tick.tick().await;
                let _ = jobs::heartbeat(&self.db, job.id, None).await;
            }
        };
        let result = tokio::select! {
            result = &mut task => result.unwrap_or_else(|e| Err(anyhow::anyhow!("job panicked: {e}"))),
            () = heartbeat => unreachable!("heartbeat loop never ends"),
        };

        self.running.lock().expect("jobs lock").remove(&job.id);

        match result {
            Ok(()) => {
                if let Err(err) = jobs::complete(&self.db, job.id).await {
                    tracing::error!(%err, id = job.id, "marking job done failed");
                }
                self.publish(&job, "done", Some(1.0));
            }
            // Server shutdown: leave it `running`; the next start requeues it.
            Err(_) if self.shutdown.is_cancelled() => {}
            // Cancelled by a user: the row is already `cancelled`.
            Err(_) if cancel.is_cancelled() => self.publish(&job, "cancelled", None),
            Err(err) => {
                let factor = 1u32 << (job.attempts.clamp(1, 7) - 1) as u32;
                let delay = (self.retry_backoff * factor).min(Duration::from_secs(3600));
                let retry_at = flixnet_db::now_ms() + delay.as_millis() as i64;
                let message = format!("{err:#}");
                match jobs::fail(&self.db, job.id, &message, retry_at).await {
                    Ok(state) => {
                        // Idle dispatchers may be sleeping on the long poll; let them
                        // re-read the next due time so the retry happens on schedule.
                        if state == "queued" {
                            self.queue.notify.notify_waiters();
                        }
                        tracing::warn!(id = job.id, kind = %job.kind, attempt = job.attempts, %state, error = %message, "job failed");
                        self.publish(&job, &state, None);
                    }
                    Err(err) => tracing::error!(%err, id = job.id, "recording job failure failed"),
                }
            }
        }
    }

    fn publish(&self, job: &jobs::Job, state: &str, progress: Option<f64>) {
        self.events.publish(ServerEvent::JobUpdated {
            id: job.id,
            kind: job.kind.clone(),
            pool: job.pool.clone(),
            state: state.to_owned(),
            progress,
        });
    }
}

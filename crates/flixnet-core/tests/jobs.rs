use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flixnet_core::events::EventBus;
use flixnet_core::jobs::{JobContext, JobHandler, JobOptions, JobRuntime, Pool};
use flixnet_core::scheduler::{TaskDef, TaskRegistry};
use flixnet_db::{Db, jobs};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

async fn open_db() -> (TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("flixnet.db")).await.unwrap();
    (dir, db)
}

/// Polls until `check` is true or the timeout hits.
async fn wait_until<F, Fut>(timeout: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    while !check().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition not met within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn count_state(db: &Db, state: &str) -> usize {
    jobs::list(db, Some(state), 1000).await.unwrap().len()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Item {
    n: u32,
}

/// Records the order in which payloads ran.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<u32>>>);

impl JobHandler for Recorder {
    const KIND: &'static str = "test.record";
    const POOL: Pool = Pool::Io;
    type Payload = Item;

    async fn run(&self, _ctx: JobContext, payload: Item) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(payload.n);
        Ok(())
    }
}

#[tokio::test]
async fn jobs_run_by_priority() {
    let (_dir, db) = open_db().await;
    let recorder = Recorder::default();
    let runtime = JobRuntime::new(db.clone(), EventBus::new())
        .register(recorder.clone())
        .concurrency(Pool::Io, 1);
    let queue = runtime.handle().queue().clone();

    // Enqueued before start, so the single worker sees all of them at once.
    for (n, priority) in [(1, 10), (2, 100), (3, 50), (4, 100)] {
        queue
            .enqueue::<Recorder>(&Item { n }, JobOptions::priority(priority))
            .await
            .unwrap();
    }
    let running = runtime.start().await.unwrap();
    wait_until(Duration::from_secs(5), || async {
        count_state(&db, "done").await == 4
    })
    .await;
    assert_eq!(*recorder.0.lock().unwrap(), vec![2, 4, 3, 1]);
    running.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn enqueue_wakes_an_idle_worker_immediately() {
    let (_dir, db) = open_db().await;
    let recorder = Recorder::default();
    let running = JobRuntime::new(db.clone(), EventBus::new())
        .register(recorder.clone())
        .start()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await; // dispatcher is now idle

    let started = tokio::time::Instant::now();
    running
        .handle()
        .queue()
        .enqueue::<Recorder>(&Item { n: 7 }, JobOptions::default())
        .await
        .unwrap();
    wait_until(Duration::from_secs(4), || async {
        count_state(&db, "done").await == 1
    })
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "worker should not wait for the idle poll"
    );
    running.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn dedupe_key_blocks_duplicates_while_pending() {
    let (_dir, db) = open_db().await;
    let runtime = JobRuntime::new(db.clone(), EventBus::new()).register(Recorder::default());
    let queue = runtime.handle().queue().clone();
    let opts = || JobOptions::default().dedupe("probe:file:1");

    assert!(
        queue
            .enqueue::<Recorder>(&Item { n: 1 }, opts())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        queue
            .enqueue::<Recorder>(&Item { n: 1 }, opts())
            .await
            .unwrap()
            .is_none()
    );

    let running = runtime.start().await.unwrap();
    wait_until(Duration::from_secs(5), || async {
        count_state(&db, "done").await == 1
    })
    .await;
    // Once done, the same key may be queued again.
    assert!(
        queue
            .enqueue::<Recorder>(&Item { n: 1 }, opts())
            .await
            .unwrap()
            .is_some()
    );
    running.shutdown(Duration::from_secs(1)).await;
}

/// Fails the first two attempts.
#[derive(Clone, Default)]
struct Flaky(Arc<AtomicU32>);

impl JobHandler for Flaky {
    const KIND: &'static str = "test.flaky";
    const POOL: Pool = Pool::Cpu;
    type Payload = ();

    async fn run(&self, ctx: JobContext, _: ()) -> anyhow::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        anyhow::ensure!(
            ctx.attempt >= 3,
            "simulated failure on attempt {}",
            ctx.attempt
        );
        Ok(())
    }
}

/// Always fails.
struct Broken;

impl JobHandler for Broken {
    const KIND: &'static str = "test.broken";
    const POOL: Pool = Pool::Cpu;
    const MAX_ATTEMPTS: i64 = 2;
    type Payload = ();

    async fn run(&self, _: JobContext, _: ()) -> anyhow::Result<()> {
        anyhow::bail!("always broken")
    }
}

#[tokio::test]
async fn failures_retry_with_backoff_until_max_attempts() {
    let (_dir, db) = open_db().await;
    let flaky = Flaky::default();
    let runtime = JobRuntime::new(db.clone(), EventBus::new())
        .register(flaky.clone())
        .register(Broken)
        .retry_backoff(Duration::from_millis(20));
    let handle = runtime.handle();
    let queue = handle.queue().clone();
    let flaky_id = queue
        .enqueue::<Flaky>(&(), JobOptions::default())
        .await
        .unwrap()
        .unwrap();
    let broken_id = queue
        .enqueue::<Broken>(&(), JobOptions::default())
        .await
        .unwrap()
        .unwrap();

    let running = runtime.start().await.unwrap();
    wait_until(Duration::from_secs(5), || async {
        let flaky = jobs::get(&db, flaky_id).await.unwrap().unwrap();
        let broken = jobs::get(&db, broken_id).await.unwrap().unwrap();
        flaky.state == "done" && broken.state == "failed"
    })
    .await;

    assert_eq!(flaky.0.load(Ordering::SeqCst), 3);
    let broken = jobs::get(&db, broken_id).await.unwrap().unwrap();
    assert_eq!(broken.attempts, 2);
    assert_eq!(broken.error.as_deref(), Some("always broken"));

    // A failed job can be retried manually; workers pick it up right away.
    assert!(handle.retry(broken_id).await.unwrap());
    wait_until(Duration::from_secs(2), || async {
        let job = jobs::get(&db, broken_id).await.unwrap().unwrap();
        job.state == "failed" && job.attempts == 2
    })
    .await;
    assert!(
        !handle.retry(flaky_id).await.unwrap(),
        "done jobs are not retried"
    );
    running.shutdown(Duration::from_secs(1)).await;
}

/// Works slowly and cooperatively; counts completed runs.
#[derive(Clone, Default)]
struct Slow(Arc<AtomicU32>);

impl JobHandler for Slow {
    const KIND: &'static str = "test.slow";
    const POOL: Pool = Pool::Probe;
    type Payload = Item;

    async fn run(&self, ctx: JobContext, _: Item) -> anyhow::Result<()> {
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(250)) => {}
            () = ctx.cancelled() => anyhow::bail!("cancelled"),
        }
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Milestone 2.2 gate: kill the server mid-queue, restart, and everything finishes.
#[tokio::test]
async fn crash_mid_queue_resumes_after_restart() {
    let (_dir, db) = open_db().await;
    let slow = Slow::default();
    let first = JobRuntime::new(db.clone(), EventBus::new())
        .register(slow.clone())
        .concurrency(Pool::Probe, 2);
    let queue = first.handle().queue().clone();
    for n in 0..6 {
        queue
            .enqueue::<Slow>(&Item { n }, JobOptions::default())
            .await
            .unwrap();
    }
    let running = first.start().await.unwrap();
    wait_until(Duration::from_secs(5), || async {
        count_state(&db, "running").await == 2
    })
    .await;

    // Crash: no bookkeeping, workers die mid-job.
    running.abort();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        count_state(&db, "running").await,
        2,
        "interrupted jobs stay marked running"
    );
    assert_eq!(
        slow.0.load(Ordering::SeqCst),
        0,
        "no job finished before the crash"
    );

    // Restart with a fresh runtime on the same database.
    let second = JobRuntime::new(db.clone(), EventBus::new())
        .register(slow.clone())
        .concurrency(Pool::Probe, 3)
        .start()
        .await
        .unwrap();
    wait_until(Duration::from_secs(10), || async {
        count_state(&db, "done").await == 6
    })
    .await;
    assert_eq!(slow.0.load(Ordering::SeqCst), 6);
    let retried = jobs::list(&db, Some("done"), 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|j| j.attempts == 2)
        .count();
    assert_eq!(retried, 2, "the two interrupted jobs ran a second time");
    second.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn graceful_shutdown_requeues_running_jobs() {
    let (_dir, db) = open_db().await;
    let slow = Slow::default();
    let runtime = JobRuntime::new(db.clone(), EventBus::new())
        .register(slow.clone())
        .concurrency(Pool::Probe, 1);
    runtime
        .handle()
        .queue()
        .enqueue::<Slow>(&Item { n: 1 }, JobOptions::default())
        .await
        .unwrap();
    let running = runtime.start().await.unwrap();
    wait_until(Duration::from_secs(5), || async {
        count_state(&db, "running").await == 1
    })
    .await;

    running.shutdown(Duration::from_secs(2)).await;
    assert_eq!(
        slow.0.load(Ordering::SeqCst),
        0,
        "the job saw the cancellation"
    );
    assert_eq!(
        count_state(&db, "running").await,
        1,
        "left for the next start"
    );

    let again = JobRuntime::new(db.clone(), EventBus::new())
        .register(slow.clone())
        .start()
        .await
        .unwrap();
    wait_until(Duration::from_secs(5), || async {
        count_state(&db, "done").await == 1
    })
    .await;
    again.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn cancelling_a_running_job_stops_it() {
    let (_dir, db) = open_db().await;
    let slow = Slow::default();
    let runtime = JobRuntime::new(db.clone(), EventBus::new()).register(slow.clone());
    let handle = runtime.handle();
    let id = handle
        .queue()
        .enqueue::<Slow>(&Item { n: 1 }, JobOptions::default())
        .await
        .unwrap()
        .unwrap();
    let running = runtime.start().await.unwrap();
    wait_until(Duration::from_secs(5), || async {
        handle.running_count() == 1
    })
    .await;

    assert!(handle.cancel(id).await.unwrap());
    wait_until(Duration::from_secs(5), || async {
        handle.running_count() == 0
    })
    .await;
    assert_eq!(
        jobs::get(&db, id).await.unwrap().unwrap().state,
        "cancelled"
    );
    assert_eq!(slow.0.load(Ordering::SeqCst), 0);
    running.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn scheduled_tasks_enqueue_their_job_when_due() {
    let (_dir, db) = open_db().await;
    let recorder = Recorder::default();
    let runtime = JobRuntime::new(db.clone(), EventBus::new()).register(recorder.clone());
    let queue = runtime.handle().queue().clone();
    let running = runtime.start().await.unwrap();

    let registry = TaskRegistry::new(vec![TaskDef::new::<Recorder>(
        "record_42",
        "test task",
        Duration::from_millis(100),
        &Item { n: 42 },
    )]);
    let stop = CancellationToken::new();
    let scheduler = registry
        .clone()
        .start(
            db.clone(),
            queue.clone(),
            Duration::from_millis(50),
            stop.clone(),
        )
        .await
        .unwrap();

    wait_until(Duration::from_secs(5), || async {
        recorder.0.lock().unwrap().len() >= 2
    })
    .await;
    assert!(recorder.0.lock().unwrap().iter().all(|&n| n == 42));
    let task = &jobs::list_tasks(&db).await.unwrap()[0];
    assert_eq!(task.name, "record_42");
    assert!(task.last_run.is_some());

    // "Run now" from the API.
    assert!(registry.run_now(&db, &queue, "record_42").await.unwrap());
    assert!(!registry.run_now(&db, &queue, "unknown").await.unwrap());

    stop.cancel();
    scheduler.await.unwrap();
    running.shutdown(Duration::from_secs(1)).await;
}

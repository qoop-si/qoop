//! The daemon's blocking work: three pools, abandonment, and degraded prefixes.
//!
//! ADR-011. One pool was the first version and it is wrong in a way that only shows up on
//! the day it matters: a probe stuck on a dead mount takes the permits that spawning and
//! teardown need, so one hung filesystem becomes a frozen daemon rather than a degraded
//! feature.
//!
//! Three mechanisms, and each one exists for a failure the others do not cover.
//!
//! **Classified pools.** A stuck job can only consume its own class's capacity. Spawning and
//! teardown always have a thread available, because a terminal that cannot be torn down is
//! a process tree nobody can reach.
//!
//! **Abandonment, not cancellation.** A blocked syscall cannot be cancelled. So the caller
//! gets its timeout immediately, the thread stays occupied until the kernel lets go, and the
//! job is counted. Pretending the thread came back would mean handing out a permit that does
//! not exist.
//!
//! **Degraded prefixes.** Abandonment alone lets one dead mount drain a pool over minutes,
//! one abandoned job at a time. After enough abandonments against a path prefix, further
//! probes against it are refused without occupying a thread at all.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// What a piece of blocking work is for. The class decides which capacity it spends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pool {
    /// Process spawn, teardown, reap. Never shared with anything that touches a path the
    /// user controls.
    Lifecycle,
    /// SQLite and history segments. Ours, and on a filesystem we chose.
    Store,
    /// Anything touching a path the user controls: a working directory, a git status, a
    /// directory walk, a capture. This is the one that hangs.
    Probe,
    /// Reading an agent's session file for a chat view, and the watch over it. Its own, so a
    /// flood of chat reads or a hung agent directory cannot take a probe thread, and the
    /// other way round. See [`chat`].
    Chat,
}

/// Concurrent jobs per pool.
///
/// Small and fixed for the two that must always work, generous for the one that can hang,
/// and the total is a constant rather than a function of the workload: gate G2.1 asserts
/// that a hundred terminals do not grow the thread count.
pub const LIFECYCLE_THREADS: usize = 2;
pub const STORE_THREADS: usize = 2;
pub const PROBE_THREADS: usize = 4;
pub const CHAT_THREADS: usize = 2;

/// The most pooled blocking threads the daemon will ever have at once. The one exception
/// is [`probe_asked`], which runs a check a person is waiting on outside the pool while
/// the pool is held by stuck probes: one thread per such request, bounded by a person's
/// pace and by the path degrading after `DEGRADE_AFTER` of them.
pub const MAX_BLOCKING_THREADS: usize =
    LIFECYCLE_THREADS + STORE_THREADS + PROBE_THREADS + CHAT_THREADS;

/// How many abandoned probes against one prefix before it is refused outright.
pub const DEGRADE_AFTER: u64 = 3;

/// How long the chat pool refuses work after [`DEGRADE_AFTER`] deadline misses in a row. Unlike
/// a prefix it recovers by itself: the chat is a view, and a mount that came back should not
/// need a daemon restart to show it again.
pub const CHAT_RECOVERY: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum BlockingError {
    #[error("the blocking pool is shut down")]
    Closed,
    #[error("the call did not finish within {0:?} and was abandoned")]
    Abandoned(Duration),
    #[error("{0} is degraded and is not being probed")]
    Degraded(String),
    /// Every probe thread is taken. Refused at once and not counted: the job never ran, so
    /// it says nothing about its own path.
    #[error("every probe thread is busy")]
    Busy,
    #[error("the call panicked")]
    Panicked,
}

struct Pools {
    lifecycle: tokio::sync::Semaphore,
    store: tokio::sync::Semaphore,
    probe: tokio::sync::Semaphore,
    chat: tokio::sync::Semaphore,
    chat_health: Mutex<ChatHealth>,
    /// Abandoned probes per path prefix, and the total.
    abandoned_by_prefix: Mutex<BTreeMap<String, u64>>,
    abandoned: AtomicU64,
}

fn pools() -> &'static Pools {
    static POOLS: std::sync::OnceLock<Pools> = std::sync::OnceLock::new();
    POOLS.get_or_init(|| Pools {
        lifecycle: tokio::sync::Semaphore::new(LIFECYCLE_THREADS),
        store: tokio::sync::Semaphore::new(STORE_THREADS),
        probe: tokio::sync::Semaphore::new(PROBE_THREADS),
        chat: tokio::sync::Semaphore::new(CHAT_THREADS),
        chat_health: Mutex::new(ChatHealth::default()),
        abandoned_by_prefix: Mutex::new(BTreeMap::new()),
        abandoned: AtomicU64::new(0),
    })
}

fn semaphore(pool: Pool) -> &'static tokio::sync::Semaphore {
    let p = pools();
    match pool {
        Pool::Lifecycle => &p.lifecycle,
        Pool::Store => &p.store,
        Pool::Probe => &p.probe,
        Pool::Chat => &p.chat,
    }
}

/// How many probes have been abandoned, in total.
pub fn abandoned_count() -> u64 {
    pools().abandoned.load(Ordering::Relaxed)
}

/// Whether a prefix has been marked degraded.
pub fn is_degraded(prefix: &str) -> bool {
    pools()
        .abandoned_by_prefix
        .lock()
        .unwrap()
        .iter()
        // By path component: a degraded `/Volumes/nas/a` says nothing about `/Volumes/nas/ab`.
        .any(|(p, n)| *n >= DEGRADE_AFTER && std::path::Path::new(prefix).starts_with(p))
}

/// Every prefix currently marked degraded, for status.
pub fn degraded_prefixes() -> Vec<String> {
    pools()
        .abandoned_by_prefix
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, n)| **n >= DEGRADE_AFTER)
        .map(|(p, _)| p.clone())
        .collect()
}

/// Forget a prefix's abandonment history, for a caller that has reason to believe it is
/// back. Nothing does this automatically: a mount that hung once will hang again, and
/// retrying on a timer is how a pool drains slowly instead of quickly.
pub fn clear_degraded(prefix: &str) {
    pools().abandoned_by_prefix.lock().unwrap().remove(prefix);
}

/// Run `f` on a pool, with no deadline.
pub async fn run_on<T, F>(pool: Pool, f: F) -> Result<T, BlockingError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let permit = semaphore(pool)
        .acquire()
        .await
        .map_err(|_| BlockingError::Closed)?;
    // The permit goes with the job, not with this future. A caller that stops waiting
    // (`run_within`'s deadline) drops the future while the thread is still blocked, and a
    // permit released then is a thread handed out twice: a hung database grew one blocked
    // thread per store call.
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await
    .map_err(|_| BlockingError::Panicked)
}

/// The store pool, which is where metadata and history go.
pub async fn run<T, F>(f: F) -> Result<T, BlockingError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    run_on(Pool::Store, f).await
}

/// The same, with a deadline covering the wait for a permit as well as the call.
pub async fn run_within<T, F>(within: Duration, f: F) -> Result<T, BlockingError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::time::timeout(within, run(f))
        .await
        .map_err(|_| BlockingError::Abandoned(within))?
}

/// Probe a path the user controls.
///
/// The thread is not recovered on a timeout, and cannot be: the syscall it is inside owns
/// it until the kernel returns. What happens instead is that the caller is told, the job is
/// counted against its prefix, and enough of them make the prefix degraded so that the next
/// caller is refused without spending a thread at all.
pub async fn probe<T, F>(prefix: &str, within: Duration, f: F) -> Result<T, BlockingError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    if is_degraded(prefix) {
        return Err(BlockingError::Degraded(prefix.to_string()));
    }
    // A permit is waited for inside the same deadline: a create behind a few quick probes
    // takes its turn. Only a pool held for the whole deadline refuses, as `Busy`, and that
    // is not counted against this path, because this path never ran.
    let deadline = tokio::time::Instant::now() + within;
    let permit = tokio::time::timeout_at(deadline, semaphore(Pool::Probe).acquire())
        .await
        .map_err(|_| BlockingError::Busy)?
        .map_err(|_| BlockingError::Closed)?;
    let result = run_probe(permit, deadline, within, f).await;
    if matches!(result, Err(BlockingError::Abandoned(_))) {
        note_abandoned(prefix);
    }
    result
}

/// A probe a person is waiting on, which never queues behind others.
///
/// A free probe thread runs it. With every probe thread held by stuck probes it runs on a
/// thread of its own instead, with the same deadline: stuck probes on one dead mount could
/// otherwise hold the whole pool, and then no tab could be opened in any directory, a
/// healthy home included, for the rest of the daemon's life. That costs one thread per
/// such request, which a person's pace bounds, and it is counted against its path either
/// way, so a dead path still degrades and is then refused without a thread at all.
pub async fn probe_asked<T, F>(prefix: &str, within: Duration, f: F) -> Result<T, BlockingError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    if is_degraded(prefix) {
        return Err(BlockingError::Degraded(prefix.to_string()));
    }
    let result = match semaphore(Pool::Probe).try_acquire() {
        Ok(permit) => run_probe(permit, tokio::time::Instant::now() + within, within, f).await,
        Err(_) => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            // A system out of threads is a busy pool, not a panic in the control handler.
            if std::thread::Builder::new()
                .spawn(move || {
                    let _ = tx.send(f());
                })
                .is_err()
            {
                return Err(BlockingError::Busy);
            }
            match tokio::time::timeout(within, rx).await {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(_)) => Err(BlockingError::Panicked),
                Err(_) => {
                    pools().abandoned.fetch_add(1, Ordering::Relaxed);
                    Err(BlockingError::Abandoned(within))
                }
            }
        }
    };
    if matches!(result, Err(BlockingError::Abandoned(_))) {
        note_abandoned(prefix);
    }
    result
}

/// Count one abandoned probe against a path, which is what degrades it.
fn note_abandoned(prefix: &str) {
    *pools()
        .abandoned_by_prefix
        .lock()
        .unwrap()
        .entry(prefix.to_string())
        .or_insert(0) += 1;
}

/// The same on the probe pool, counted against no prefix: for work that asks about no one
/// path, where a refusal means an absent answer rather than a failure.
pub async fn try_probe<T, F>(within: Duration, f: F) -> Result<T, BlockingError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    // Taken now or not at all (ADR-011: jobs are rejected when the pool is full): what
    // this answers is optional, and waiting behind stuck probes would only delay the
    // call it is part of.
    let permit = semaphore(Pool::Probe)
        .try_acquire()
        .map_err(|_| BlockingError::Busy)?;
    run_probe(permit, tokio::time::Instant::now() + within, within, f).await
}

/// Consecutive deadline misses of the chat pool, and until when it refuses work.
#[derive(Default)]
struct ChatHealth {
    misses: u64,
    until: Option<std::time::Instant>,
}

/// Whether the chat pool is refusing work at `now`. Past its time the pool is trusted again
/// with a clean count.
fn chat_degraded(now: std::time::Instant) -> bool {
    let mut health = pools().chat_health.lock().unwrap();
    match health.until {
        Some(until) if now < until => true,
        Some(_) => {
            *health = ChatHealth::default();
            false
        }
        None => false,
    }
}

/// A chat job that finished is a mount that answers: the run of misses ends.
fn note_chat_answered() {
    pools().chat_health.lock().unwrap().misses = 0;
}

/// A chat job that outlived its deadline. Only this counts toward degrading: a refusal for a
/// busy pool says nothing about the disk.
fn note_chat_missed(now: std::time::Instant) {
    let mut health = pools().chat_health.lock().unwrap();
    health.misses += 1;
    if health.misses >= DEGRADE_AFTER {
        health.until = Some(now + CHAT_RECOVERY);
    }
}

/// Read an agent's session on the chat pool: two threads, taken now or not at all.
///
/// No permit free is `Busy` at once, and never a queue or a thread of its own, so a flood of
/// chat reads costs the daemon two threads and a refusal each. Deadline misses count against
/// the chat alone and degrade it for [`CHAT_RECOVERY`]; a busy refusal never does.
pub async fn chat<T, F>(within: Duration, f: F) -> Result<T, BlockingError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    if chat_degraded(std::time::Instant::now()) {
        return Err(BlockingError::Degraded("chat".into()));
    }
    let permit = semaphore(Pool::Chat)
        .try_acquire()
        .map_err(|_| BlockingError::Busy)?;
    let result = run_probe(permit, tokio::time::Instant::now() + within, within, f).await;
    match &result {
        Err(BlockingError::Abandoned(_)) => note_chat_missed(std::time::Instant::now()),
        Ok(_) => note_chat_answered(),
        Err(_) => {}
    }
    result
}

/// Run `f` holding a probe permit until `deadline`.
async fn run_probe<T, F>(
    permit: tokio::sync::SemaphorePermit<'static>,
    deadline: tokio::time::Instant,
    within: Duration,
    f: F,
) -> Result<T, BlockingError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    // The permit is held by the blocking job rather than by this future, so that a timeout
    // here does not release capacity the blocked thread is still using.
    let handle = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    });
    match tokio::time::timeout_at(deadline, handle).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(BlockingError::Panicked),
        Err(_) => {
            pools().abandoned.fetch_add(1, Ordering::Relaxed);
            Err(BlockingError::Abandoned(within))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    /// The probe pool is one per process, and a job a test abandoned keeps its permit until
    /// its sleep ends. So a probe test takes this, and waits for the pool to drain, or one
    /// test's stuck jobs are another test's busy pool.
    async fn exclusive_probe_pool() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let held = LOCK.lock().await;
        while semaphore(Pool::Probe).available_permits() < PROBE_THREADS {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        held
    }

    fn unique(prefix: &str) -> String {
        format!(
            "{prefix}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_pool_bounds_only_itself() {
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..32 {
            let live = live.clone();
            let peak = peak.clone();
            handles.push(tokio::spawn(async move {
                let _ = run_on(Pool::Store, move || {
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    live.fetch_sub(1, Ordering::SeqCst);
                })
                .await;
            }));
        }
        for h in handles {
            h.await.expect("joined");
        }
        assert!(
            peak.load(Ordering::SeqCst) <= STORE_THREADS,
            "{} store jobs ran at once, limit {STORE_THREADS}",
            peak.load(Ordering::SeqCst)
        );
    }

    /// The failure one pool would have: a stuck probe taking the capacity that spawning and
    /// teardown need.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stuck_probe_does_not_starve_lifecycle_work() {
        let _pool = exclusive_probe_pool().await;
        let prefix = unique("/hung");
        // Fill the probe pool with jobs that will not return for a long time.
        for _ in 0..(PROBE_THREADS * 2) {
            let prefix = prefix.clone();
            tokio::spawn(async move {
                let _ = probe(&prefix, Duration::from_millis(50), || {
                    std::thread::sleep(Duration::from_secs(3))
                })
                .await;
            });
        }
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Lifecycle work still runs, promptly.
        let started = std::time::Instant::now();
        run_on(Pool::Lifecycle, || 7u8)
            .await
            .expect("lifecycle work must always have a thread");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "lifecycle work waited {:?} behind a hung probe",
            started.elapsed()
        );
        // And so does store work.
        run_on(Pool::Store, || 7u8).await.expect("store work");
    }

    /// Abandonment: the caller is told, and the count grows.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_probe_that_does_not_return_is_abandoned_and_counted() {
        let _pool = exclusive_probe_pool().await;
        let prefix = unique("/slow");
        let before = abandoned_count();
        let result = probe(&prefix, Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_secs(2))
        })
        .await;
        assert!(matches!(result, Err(BlockingError::Abandoned(_))));
        assert!(
            abandoned_count() > before,
            "an abandoned probe was not counted"
        );
    }

    /// And enough of them stop the next one from spending a thread at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn enough_abandonments_mark_the_prefix_degraded() {
        let _pool = exclusive_probe_pool().await;
        let prefix = unique("/dead");
        for _ in 0..DEGRADE_AFTER {
            let _ = probe(&prefix, Duration::from_millis(30), || {
                std::thread::sleep(Duration::from_secs(2))
            })
            .await;
        }
        assert!(is_degraded(&prefix), "the prefix was not marked degraded");
        assert!(degraded_prefixes().iter().any(|p| p == &prefix));

        // The next call is refused immediately, without waiting for a deadline.
        let started = std::time::Instant::now();
        let result = probe(&prefix, Duration::from_secs(30), || 1u8).await;
        assert!(matches!(result, Err(BlockingError::Degraded(_))));
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "a degraded prefix still cost a deadline: {:?}",
            started.elapsed()
        );

        // A path under the degraded prefix is refused too, and an unrelated one is not.
        assert!(is_degraded(&format!("{prefix}/deeper")));
        assert!(!is_degraded(&unique("/fine")));

        clear_degraded(&prefix);
        assert!(!is_degraded(&prefix));
    }

    /// A full pool is a refusal, not an abandonment. A healthy path queued behind four stuck
    /// probes never ran, so counting it would mark a prefix degraded for another's hang.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_probe_refused_by_a_full_pool_is_busy_and_not_counted() {
        let _pool = exclusive_probe_pool().await;
        let stuck = unique("/stuck");
        for _ in 0..PROBE_THREADS {
            let stuck = stuck.clone();
            tokio::spawn(async move {
                let _ = probe(&stuck, Duration::from_millis(50), || {
                    std::thread::sleep(Duration::from_secs(1))
                })
                .await;
            });
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        let healthy = unique("/healthy");
        let result = probe(&healthy, Duration::from_millis(50), || 1u8).await;
        assert!(matches!(result, Err(BlockingError::Busy)), "{result:?}");
        assert!(!is_degraded(&healthy));
        assert!(
            !pools()
                .abandoned_by_prefix
                .lock()
                .unwrap()
                .contains_key(&healthy),
            "a probe that never ran was counted against its prefix"
        );
        clear_degraded(&stuck);
    }

    /// A path behind a pool of quick probes waits its turn inside its deadline. Refused at
    /// once, four tabs opened together made the fifth fail.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_probe_behind_quick_ones_gets_its_turn() {
        let _pool = exclusive_probe_pool().await;
        let quick = unique("/quick");
        for _ in 0..PROBE_THREADS {
            let quick = quick.clone();
            tokio::spawn(async move {
                let _ = probe(&quick, Duration::from_secs(2), || {
                    std::thread::sleep(Duration::from_millis(50))
                })
                .await;
            });
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        let waiting = unique("/waiting");
        let result = probe(&waiting, Duration::from_secs(2), || 7u8).await;
        assert!(matches!(result, Ok(7)), "{result:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deadline_covers_the_wait_for_a_permit_too() {
        for _ in 0..STORE_THREADS {
            tokio::spawn(run(|| std::thread::sleep(Duration::from_millis(500))));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let result = run_within(Duration::from_millis(100), || 7u8).await;
        assert!(matches!(result, Err(BlockingError::Abandoned(_))));
    }

    /// The chat pool is one test's: its permits and health are the process's.
    static CHAT_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn chat_idle() {
        while semaphore(Pool::Chat).available_permits() < CHAT_THREADS {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Two threads, and the third request is refused on the spot: no queue, no thread of its
    /// own, no mark against the disk, and the probe pool is not touched.
    #[tokio::test]
    async fn the_chat_pool_refuses_at_once_when_both_threads_are_taken() {
        let _only = CHAT_TESTS.lock().await;
        chat_idle().await;
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Lets the held jobs go when the test ends however it ends, or a failed assertion
        // leaves them spinning and the runtime waiting for them.
        struct Release(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let _let_go = Release(release.clone());
        let mut held = Vec::new();
        for _ in 0..CHAT_THREADS {
            let release = release.clone();
            held.push(tokio::spawn(chat(Duration::from_secs(10), move || {
                while !release.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            })));
        }
        while semaphore(Pool::Chat).available_permits() > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let started = std::time::Instant::now();
        for _ in 0..50 {
            let refused = chat(Duration::from_secs(10), || ()).await;
            assert!(matches!(refused, Err(BlockingError::Busy)), "{refused:?}");
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a refusal waited"
        );
        // Fifty busy answers are not a disk that hangs.
        assert!(!chat_degraded(std::time::Instant::now()));
        release.store(true, Ordering::Relaxed);
        for job in held {
            assert!(job.await.unwrap().is_ok());
        }
        chat_idle().await;
        assert!(chat(Duration::from_secs(1), || 7).await.is_ok());
    }

    /// A disk that keeps missing its deadline degrades the chat, and only the chat, then comes
    /// back by itself.
    #[tokio::test]
    async fn the_chat_pool_degrades_on_repeated_deadline_misses_and_recovers_by_itself() {
        let _only = CHAT_TESTS.lock().await;
        chat_idle().await;
        for _ in 0..DEGRADE_AFTER {
            let missed = chat(Duration::from_millis(20), || {
                std::thread::sleep(Duration::from_millis(120))
            })
            .await;
            assert!(
                matches!(missed, Err(BlockingError::Abandoned(_))),
                "{missed:?}"
            );
            chat_idle().await;
        }
        let refused = chat(Duration::from_secs(1), || ()).await;
        assert!(
            matches!(refused, Err(BlockingError::Degraded(_))),
            "{refused:?}"
        );
        assert!(
            tokio::task::spawn_blocking(|| ()).await.is_ok(),
            "an ordinary blocking call is held up by the chat"
        );
        assert!(
            !is_degraded("chat"),
            "the chat is a prefix of the probe pool's"
        );
        // Thirty seconds later it is trusted again, with a clean count.
        let later = std::time::Instant::now() + CHAT_RECOVERY + Duration::from_secs(1);
        assert!(!chat_degraded(later));
        assert!(chat(Duration::from_secs(1), || ()).await.is_ok());
    }
}

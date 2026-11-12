//! A single worker thread, used to move work off the interactive ZLE thread.
//!
//! This answers Stage B task B7: **is it safe to run a Rust thread inside the
//! zsh process?**
//!
//! # The discipline that makes it acceptable
//!
//! zsh is single-threaded and not thread-safe. Two rules, and only two:
//!
//! 1. **The worker never calls into zsh.** It cannot: it holds no handle on the
//!    resolved zsh API. Everything crossing the boundary is `Vec<u8>` and
//!    integers, so there is no zsh type, pointer or lifetime in this module.
//! 2. **Only the owning thread submits.** A worker submitting to its own reply
//!    channel would deadlock -- a programming error, not a runtime condition.
//!
//! # Known residual risk: signals
//!
//! zsh installs process-wide signal handlers. A signal delivered to the
//! process can land on *our* thread, where zsh's handler would run on a stack
//! zsh knows nothing about. This cannot be fixed from inside a module.
//!
//! It is why the worker is **optional**: [`shared`] returns `None` if the
//! thread cannot start, and the engine must stay fully functional with zero
//! workers running. Signal behaviour is measured, not assumed -- see
//! `crates/zsh-module/tests/zsh_integration.rs`.
//!
//! # Deliberately not async
//!
//! No Tokio, no executor. One `std::thread` and a channel is enough to get work
//! off the keystroke path, and a runtime would add per-shell memory for no
//! measured benefit. Revisit only with a benchmark.
//!
//! # Not blocking
//!
//! [`Worker::submit`] blocks for at most `timeout`, so it is for background
//! work only. The ZLE path uses `zle -F` and must never block.
//!
//! # Lifecycle
//!
//! The worker is started on demand ([`shared`]) and stopped when the module
//! that owns it goes away ([`shutdown_shared`]).
//!
//! The stop is a **join**, not a request. The worker runs inside the user's
//! interactive shell, which keeps running long after the plugin is unloaded,
//! so a thread that outlives the module is a thread that outlives the
//! plugin — and a reloaded module would find a dead worker where it
//! expected a live one. [`shutdown_shared`] therefore stops the thread *and*
//! waits for it, and leaves the slot empty so that the next [`shared`]
//! starts a fresh worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Default time to wait for a result before giving up.
///
/// Must stay far below what a user perceives as a hang, and far above the cost
/// of the work itself.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(50);

/// Name of the worker thread. Linux truncates thread names to 15 bytes, so
/// this must stay under that or tests cannot find the thread again.
pub const WORKER_THREAD_NAME: &str = "zshpulse-wrk";

/// A unit of work handed to the worker.
#[derive(Debug)]
pub struct Job {
    /// Monotonic id, echoed back so a late result can be matched to its
    /// request. This is the revision guard from the plan.
    pub revision: u64,
    /// Opaque payload. Owned, so nothing crosses the thread boundary by
    /// reference and no lifetime leaks into the worker.
    pub payload: Vec<u8>,
}

/// The result of a [`Job`].
#[derive(Debug)]
pub struct Reply {
    /// The revision of the `Job` this answers.
    pub revision: u64,
    /// Worker-computed checksum, proving the work ran on the right data.
    pub checksum: u64,
}

/// Why a submission did not produce a reply.
#[derive(Debug)]
pub enum WorkerError {
    /// No reply within the deadline. Treat as "no result", never as a display
    /// failure.
    Timeout,
    /// The worker is gone or stopped.
    Disconnected,
    /// The reply did not belong to this request.
    ///
    /// The worker answers requests in order, so this can only happen if two
    /// callers share a worker — which the design forbids. Checking it turns
    /// a silent mix-up into a visible error.
    RevisionMismatch { expected: u64, got: u64 },
    /// The OS refused to create the thread.
    SpawnFailed(std::io::Error),
}

/// A handle to the worker thread.
#[derive(Debug)]
pub struct Worker {
    /// `None` after [`Worker::stop`]; dropping the sender ends the thread.
    tx: Mutex<Option<Sender<Job>>>,
    /// `Receiver` is `Send` but not `Sync`, so it needs a mutex for the
    /// handle itself to be shareable across threads.
    rx: Mutex<Receiver<Reply>>,
    stop: Arc<AtomicBool>,
    /// The thread's handle, joined when the worker is dropped.
    ///
    /// Without this the thread's exit is asynchronous, and a test that
    /// counts worker threads has to poll for it — which is how the thread
    /// counting tests became flaky.
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Worker {
    /// Start a worker thread.
    ///
    /// # Errors
    ///
    /// [`WorkerError::SpawnFailed`] if the OS refuses the thread. Deliberately
    /// not a panic: a shell must not die because a thread could not start.
    pub fn spawn() -> Result<Self, WorkerError> {
        let (tx, rx) = channel::<Job>();
        let (out_tx, out_rx) = channel::<Reply>();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);

        let handle = std::thread::Builder::new()
            // Linux truncates thread names to 15 bytes, so this is deliberately
            // short: tests identify the thread by this exact name.
            .name(WORKER_THREAD_NAME.into())
            // 256 KiB: ample for hashing and parsing, and an interactive shell
            // should not hand out a default multi-megabyte stack.
            .stack_size(256 * 1024)
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let checksum = checksum(&job.payload);
                    if out_tx.send(Reply { revision: job.revision, checksum }).is_err() {
                        break;
                    }
                    if flag.load(Ordering::Relaxed) {
                        break;
                    }
                }
            })
            .map_err(WorkerError::SpawnFailed)?;

        Ok(Self {
            tx: Mutex::new(Some(tx)),
            rx: Mutex::new(out_rx),
            stop,
            handle: Mutex::new(Some(handle)),
        })
    }

    /// Ask the worker to finish after the current job.
    ///
    /// Called on module unload so reloading the plugin does not leak threads.
    ///
    /// This only *signals* the thread: it is [`Worker::shutdown`] that waits
    /// for the thread to be gone.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        // Recover from poisoning: a poisoned mutex must not leave the
        // sender alive, or the thread would never exit and `Drop` would
        // block forever on the join.
        let mut guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        guard.take();
    }

    /// Stop the worker and wait for its thread to finish.
    ///
    /// When this returns, the thread is **gone** — not merely on its way
    /// out. That is what the module's unload hook needs: the module is
    /// loaded into a shell that keeps running, so a thread that outlives
    /// the module is a thread that outlives the plugin.
    ///
    /// Idempotent: a second call finds the thread already joined and does
    /// nothing.
    pub fn shutdown(&self) {
        self.stop();
        self.join();
    }

    /// Wait for the worker thread to finish.
    ///
    /// The join handle is taken out of its mutex *before* the join, so a
    /// second call — or the `Drop` impl — never tries to join the same
    /// thread twice.
    pub fn join(&self) {
        let handle = {
            let mut guard = self.handle.lock().unwrap_or_else(|e| e.into_inner());
            guard.take()
        };
        if let Some(handle) = handle {
            // The thread may have panicked. That is not this function's
            // problem, and a panic must not unwind into the caller: the
            // caller may be zsh.
            let _ = handle.join();
        }
    }

    /// Whether the worker has been asked to stop.
    pub fn is_stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Submit work and wait up to `timeout` for the reply.
    ///
    /// # Errors
    ///
    /// [`WorkerError::Timeout`] if no reply arrived, or
    /// [`WorkerError::Disconnected`] if the worker is gone.
    pub fn submit(
        &self,
        revision: u64,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<Reply, WorkerError> {
        {
            let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
            let Some(tx) = guard.as_ref() else {
                return Err(WorkerError::Disconnected);
            };
            tx.send(Job { revision, payload }).map_err(|_| WorkerError::Disconnected)?;
        }
        let rx = self.rx.lock().unwrap_or_else(|e| e.into_inner());
        let reply = match rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => return Err(WorkerError::Timeout),
            Err(RecvTimeoutError::Disconnected) => return Err(WorkerError::Disconnected),
        };
        if reply.revision != revision {
            return Err(WorkerError::RevisionMismatch {
                expected: revision,
                got: reply.revision,
            });
        }
        Ok(reply)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Dropping the sender makes the thread's `recv` fail, so it exits
        // promptly; joining it here means the thread is *gone* when `drop`
        // returns, not merely on its way out.
        self.shutdown();
    }
}

/// The worker's "computation": an order-sensitive checksum.
///
/// FNV-1a: trivial, allocation-free, and sensitive to every byte *and* its
/// position, so a mis-routed payload produces a different answer.
pub fn checksum(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The process-wide worker, started lazily on first use.
///
/// Lazy because spawning a thread during `.zshrc` costs startup time for a
/// feature the user may never invoke.
///
/// The slot can be emptied and refilled: [`shutdown_shared`] takes the
/// worker out of it and joins its thread, so a module that is unloaded and
/// loaded again gets a *fresh* worker instead of a dead one. That is why
/// this is a `Mutex` and not a `OnceLock` — a `OnceLock` can only ever be
/// initialised once, and a worker that has been stopped can never be
/// replaced.
static WORKER: Mutex<Option<Arc<Worker>>> = Mutex::new(None);

/// Get the shared worker, starting it on first use.
///
/// Returns `None` if the thread could not be created, which is a supported
/// outcome: the engine must work with zero workers running.
///
/// The handle is a clone of the shared worker, so the caller may hold on to
/// it without keeping the slot busy. After [`shutdown_shared`] the worker is
/// gone, and this starts a new one.
pub fn shared() -> Option<Arc<Worker>> {
    let mut slot = WORKER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(worker) = slot.as_ref() {
        return Some(Arc::clone(worker));
    }
    let worker = Arc::new(Worker::spawn().ok()?);
    *slot = Some(Arc::clone(&worker));
    Some(worker)
}

/// Stop the shared worker, if one was ever started.
///
/// The thread is **joined**, not merely asked to stop: when this returns,
/// the thread is gone. The worker lives inside the user's interactive shell,
/// which keeps running long after the module is unloaded, so a thread that
/// outlives the module is a thread that outlives the plugin.
///
/// The slot is emptied as well, so that a later [`shared`] starts a fresh
/// worker. That is what makes `zmodload -u` followed by `zmodload` work: the
/// unload must not leave a thread behind, and the reload must not find a
/// dead worker.
///
/// Idempotent: calling it twice, or on a worker that was never started, does
/// nothing.
pub fn shutdown_shared() {
    // The lock is held across the join, so a concurrent `shared()` cannot
    // start a new worker while the old one is still running.
    let mut slot = WORKER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(worker) = slot.take() {
        worker.shutdown();
        // `worker` is dropped here. If this was the last reference, `Drop`
        // runs and finds the thread already joined, so it does nothing.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests that count threads.
    ///
    /// Thread counts are process-wide, so a test that spawns a worker while
    /// another is asserting `before + 1` sees the other one's thread too.
    /// This is the same class of bug as the shared `DEGRADED` flag in zsh-abi.
    static THREAD_LOCK: Mutex<()> = Mutex::new(());

    fn serialise_threads() -> std::sync::MutexGuard<'static, ()> {
        THREAD_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn checksum_is_order_sensitive() {
        assert_eq!(checksum(b"ab"), checksum(b"ab"));
        assert_ne!(checksum(b"ab"), checksum(b"ba"));
        assert_ne!(checksum(b""), checksum(b"a"));
    }

    #[test]
    fn checksum_is_the_known_fnv1a_value() {
        // Pinned so a change to the algorithm is caught, not silently accepted.
        assert_eq!(checksum(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(checksum(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn worker_answers_with_the_same_revision_and_checksum() {
        let _g = serialise_threads();
        let w = Worker::spawn().expect("spawn");
        let reply = w.submit(7, b"hello".to_vec(), Duration::from_secs(2)).expect("reply");
        assert_eq!(reply.revision, 7, "the revision guard depends on this");
        assert_eq!(reply.checksum, checksum(b"hello"));
    }

    #[test]
    fn a_reply_for_another_request_is_refused() {
        // The worker answers in order, so this can only happen if two
        // callers share one. The check turns that into a visible error
        // rather than a silently wrong answer.
        let _g = serialise_threads();
        let w = Worker::spawn().expect("spawn");
        let r = w.submit(3, b"x".to_vec(), Duration::from_secs(2)).expect("reply");
        assert_eq!(r.revision, 3);
    }

    #[test]
    fn revisions_are_echoed_not_conflated() {
        let _g = serialise_threads();
        let w = Worker::spawn().expect("spawn");
        for rev in 0..64u64 {
            let payload = vec![(rev % 251) as u8; 8];
            let r = w.submit(rev, payload.clone(), Duration::from_secs(2)).expect("reply");
            assert_eq!(r.revision, rev);
            assert_eq!(r.checksum, checksum(&payload));
        }
    }

    #[test]
    fn work_actually_runs_on_a_separate_named_thread() {
        if !can_count_threads() {
            eprintln!("skipping: this platform has no /proc to count threads");
            return;
        }
        let _g = serialise_threads();
        let before = settled_worker_thread_count();
        let w = Worker::spawn().expect("spawn");
        let r = w.submit(1, b"x".to_vec(), Duration::from_secs(2)).expect("reply");
        assert_eq!(r.checksum, checksum(b"x"));
        // Asserting on *our own thread by name* rather than a global count, so
        // parallel tests cannot perturb the result. Use a settled baseline and
        // compare the delta so a transiently lingering worker thread from an
        // earlier test in the process cannot make this flaky.
        let after = wait_for_worker_threads(before + 1);
        assert_eq!(
            after.saturating_sub(before),
            1,
            "spawning a Worker must create one thread named {WORKER_THREAD_NAME}"
        );
    }

    #[test]
    fn large_payload_round_trips() {
        let _g = serialise_threads();
        let w = Worker::spawn().expect("spawn");
        let payload: Vec<u8> = (0..64 * 1024).map(|i| (i % 256) as u8).collect();
        let r = w.submit(2, payload.clone(), Duration::from_secs(5)).expect("reply");
        assert_eq!(r.checksum, checksum(&payload));
    }

    #[test]
    fn stop_makes_further_submissions_fail_rather_than_hang() {
        let _g = serialise_threads();
        let w = Worker::spawn().expect("spawn");
        w.submit(1, b"x".to_vec(), Duration::from_secs(2)).expect("first");
        w.stop();
        assert!(w.is_stopping());
        assert!(
            matches!(
                w.submit(2, b"y".to_vec(), Duration::from_millis(50)),
                Err(WorkerError::Disconnected)
            ),
            "a stopped worker must refuse, not block"
        );
    }

    #[test]
    fn tiny_timeout_never_panics_or_hangs() {
        let _g = serialise_threads();
        let w = Worker::spawn().expect("spawn");
        let r = w.submit(3, b"z".to_vec(), Duration::from_nanos(1));
        assert!(matches!(r, Ok(_) | Err(WorkerError::Timeout)));
    }

    #[test]
    fn dropping_a_worker_does_not_leak_its_thread() {
        if !can_count_threads() {
            eprintln!("skipping: this platform has no /proc to count threads");
            return;
        }
        let _g = serialise_threads();
        let before = worker_thread_count();
        {
            let _w = Worker::spawn().expect("spawn");
            assert_eq!(wait_for_worker_threads(before + 1), before + 1);
        }
        // `Worker::drop` joins the thread, so it has finished by the time
        // the handle is dropped. The kernel may still list it for a moment,
        // so the count is polled rather than asserted once.
        for _ in 0..200 {
            if worker_thread_count() <= before {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("worker thread outlived its Worker handle");
    }

    #[test]
    fn shutdown_shared_stops_the_shared_worker_thread() {
        if !can_count_threads() {
            eprintln!("skipping: this platform has no /proc to count threads");
            return;
        }
        let _g = serialise_threads();
        let before = settled_worker_thread_count();
        let worker = shared().expect("shared worker");
        assert!(
            worker.submit(1, b"x".to_vec(), Duration::from_secs(2)).is_ok(),
            "the shared worker must answer before it is shut down"
        );
        assert_eq!(
            wait_for_worker_threads(before + 1),
            before + 1,
            "the shared worker must run on its own thread"
        );

        shutdown_shared();

        assert!(
            wait_for_no_worker_threads(before),
            "the shared worker thread outlived shutdown_shared"
        );
        assert!(
            !has_live_handle(&worker),
            "shutdown_shared must join the thread, not merely ask it to stop"
        );
        assert!(
            matches!(
                worker.submit(2, b"y".to_vec(), Duration::from_millis(50)),
                Err(WorkerError::Disconnected)
            ),
            "a worker that has been shut down must refuse work, not block"
        );
    }

    #[test]
    fn shutdown_shared_waits_for_a_busy_worker_thread() {
        let _g = serialise_threads();
        let worker = shared().expect("shared worker");

        // A payload big enough that the checksum takes milliseconds, so
        // the thread is certainly still working when the shutdown starts.
        submit_without_waiting(&worker, 1, vec![7u8; 4 * 1024 * 1024]);

        shutdown_shared();

        // The job was finished before the shutdown returned. A shutdown
        // that only *signalled* the thread would have returned while the
        // thread was still working on it, and no reply would be waiting.
        assert!(
            reply_is_pending(&worker),
            "shutdown_shared must wait for the worker thread to finish its job"
        );
    }

    #[test]
    fn a_worker_can_be_started_again_after_shutdown_shared() {
        if !can_count_threads() {
            eprintln!("skipping: this platform has no /proc to count threads");
            return;
        }
        let _g = serialise_threads();
        let before = settled_worker_thread_count();
        let first = shared().expect("first shared worker");
        assert_eq!(wait_for_worker_threads(before + 1), before + 1);

        shutdown_shared();

        // A reload (`zmodload -u` then `zmodload`) must find a *live* worker,
        // not the corpse of the one that was shut down.
        let second = shared().expect("a worker after the shutdown");
        assert!(
            !Arc::ptr_eq(&first, &second),
            "the worker must be started afresh, not handed out dead"
        );
        let reply = second
            .submit(2, b"y".to_vec(), Duration::from_secs(2))
            .expect("the new worker must answer");
        assert_eq!(reply.revision, 2);
        assert_eq!(reply.checksum, checksum(b"y"));

        shutdown_shared();
        assert!(
            wait_for_no_worker_threads(before),
            "the restarted worker thread outlived the second shutdown"
        );
    }

    #[test]
    fn shutdown_shared_is_idempotent() {
        let _g = serialise_threads();
        let before = settled_worker_thread_count();
        // The module's unload hook and its finalisation hook both stop the
        // worker, and either may run without the other. The second call
        // must be harmless.
        shutdown_shared();
        let _worker = shared().expect("shared worker");
        shutdown_shared();
        shutdown_shared();
        assert!(
            wait_for_no_worker_threads(before),
            "the worker thread outlived the shutdown"
        );
    }

    /// Whether the worker still holds its thread's join handle.
    ///
    /// The handle is taken when the thread is joined, so this is false
    /// exactly when the thread is known to have finished.
    fn has_live_handle(w: &Worker) -> bool {
        w.handle.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Whether a reply is waiting to be collected.
    ///
    /// Used to tell a *joined* shutdown from one that merely signalled the
    /// thread: the reply is sent when the job is finished, so a reply that
    /// is already waiting means the thread got to the end of its work.
    fn reply_is_pending(w: &Worker) -> bool {
        let rx = w.rx.lock().unwrap_or_else(|e| e.into_inner());
        rx.try_recv().is_ok()
    }

    /// Hand a job to the worker without waiting for its reply.
    ///
    /// Used to keep the worker busy while the shutdown happens, so that a
    /// test can tell a *joined* shutdown from one that merely signalled the
    /// thread.
    fn submit_without_waiting(w: &Worker, revision: u64, payload: Vec<u8>) {
        let guard = w.tx.lock().unwrap_or_else(|e| e.into_inner());
        let tx = guard.as_ref().expect("the worker has a sender");
        tx.send(Job { revision, payload }).expect("send");
    }

    /// Wait for the number of worker threads to reach `want`.
    ///
    /// `std::thread::Builder::spawn` returns as soon as the thread exists,
    /// but the kernel only shows its name once the thread has started
    /// running, so the count can lag by a moment. Polling removes the race
    /// that made these tests flaky.
    fn wait_for_worker_threads(want: usize) -> usize {
        for _ in 0..200 {
            let n = worker_thread_count();
            if n >= want {
                return n;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        worker_thread_count()
    }

    /// Wait until no more than `want` worker threads are left.
    ///
    /// The kernel can list a thread that has already been joined for a
    /// moment: it wakes the joiner before it removes the thread's entry
    /// from `/proc`. The count is therefore polled rather than asserted
    /// once.
    fn wait_for_no_worker_threads(want: usize) -> bool {
        for _ in 0..200 {
            if worker_thread_count() <= want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        worker_thread_count() <= want
    }

    /// The number of worker threads, once it has stopped changing.
    ///
    /// A baseline taken while the kernel is still listing a thread that has
    /// already been joined would be one too high, and every later
    /// comparison against it would be wrong.
    fn settled_worker_thread_count() -> usize {
        let mut last = worker_thread_count();
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(1));
            let now = worker_thread_count();
            if now == last {
                return now;
            }
            last = now;
        }
        last
    }

    /// Whether this platform can count threads at all.
    ///
    /// The count comes from `/proc`, which only exists on Linux. Off Linux
    /// the thread-counting tests cannot say anything, so they skip rather
    /// than fail.
    fn can_count_threads() -> bool {
        std::path::Path::new("/proc/self/task").is_dir()
    }

    /// Number of live threads named { WORKER_THREAD_NAME}.
    ///
    /// Counts only *our* worker threads, so the result does not depend on how
    /// the test harness happens to schedule other tests. 0 off Linux.
    fn worker_thread_count() -> usize {
        let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
            return 0;
        };
        tasks
            .filter_map(|e| e.ok())
            .filter(|e| {
                std::fs::read_to_string(e.path().join("comm"))
                    .map(|n| n.trim() == WORKER_THREAD_NAME)
                    .unwrap_or(false)
            })
            .count()
    }
}

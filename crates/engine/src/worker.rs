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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Default time to wait for a result before giving up.
///
/// Must stay far below what a user perceives as a hang, and far above the cost
/// of the work itself.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(50);

/// Name of the worker thread. Linux truncates thread names to 15 bytes, so
/// this must stay under that or tests cannot find the thread again.
pub const WORKER_THREAD_NAME: &str = "shellpulse-wrk";

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

        std::thread::Builder::new()
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

        Ok(Self { tx: Mutex::new(Some(tx)), rx: Mutex::new(out_rx), stop })
    }

    /// Ask the worker to finish after the current job.
    ///
    /// Called on module unload so reloading the plugin does not leak threads.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut guard) = self.tx.lock() {
            guard.take();
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
        match rx.recv_timeout(timeout) {
            Ok(r) => Ok(r),
            Err(RecvTimeoutError::Timeout) => Err(WorkerError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(WorkerError::Disconnected),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
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
static WORKER: OnceLock<Option<Worker>> = OnceLock::new();

/// Get the shared worker, starting it on first use.
///
/// Returns `None` if the thread could not be created, which is a supported
/// outcome: the engine must work with zero workers running.
pub fn shared() -> Option<&'static Worker> {
    WORKER.get_or_init(|| Worker::spawn().ok()).as_ref()
}

/// Stop the shared worker, if one was ever started.
pub fn shutdown_shared() {
    if let Some(w) = WORKER.get().and_then(Option::as_ref) {
        w.stop();
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
        let _g = serialise_threads();
        let before = worker_thread_count();
        let w = Worker::spawn().expect("spawn");
        let r = w.submit(1, b"x".to_vec(), Duration::from_secs(2)).expect("reply");
        assert_eq!(r.checksum, checksum(b"x"));
        // Asserting on *our own thread by name* rather than a global count, so
        // parallel tests cannot perturb the result.
        assert_eq!(
            worker_thread_count(),
            before + 1,
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
        let _g = serialise_threads();
        let before = worker_thread_count();
        {
            let _w = Worker::spawn().expect("spawn");
            assert_eq!(worker_thread_count(), before + 1);
        }
        for _ in 0..200 {
            if worker_thread_count() <= before {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("worker thread outlived its Worker handle");
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

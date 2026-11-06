//! Run blocking per-mount queries in parallel with one shared deadline, so a dead network
//! mount (hard NFS, disconnected SMB, hung FUSE) can't freeze the whole listing.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Run every job on its own thread. Results not ready by `timeout` are `None`. Stuck workers
/// are abandoned; they end with the process.
pub fn run_all<T, F>(jobs: impl IntoIterator<Item = F>, timeout: Duration) -> Vec<Option<T>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let jobs: Vec<F> = jobs.into_iter().collect();
    let mut out: Vec<Option<T>> = (0..jobs.len()).map(|_| None).collect();
    let (tx, rx) = mpsc::channel();
    let mut left = 0;
    for (i, job) in jobs.into_iter().enumerate() {
        let tx = tx.clone();
        let spawned = thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let _ = tx.send((i, job()));
            });
        if spawned.is_ok() {
            left += 1;
        }
    }
    drop(tx);

    let deadline = Instant::now() + timeout;
    while left > 0 {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((i, v)) => {
                out[i] = Some(v);
                left -= 1;
            }
            Err(_) => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_jobs_time_out_without_delaying_the_rest() {
        let start = Instant::now();
        let out = run_all(
            [0u64, 5_000, 10, 5_000].map(|ms| {
                move || {
                    thread::sleep(Duration::from_millis(ms));
                    ms
                }
            }),
            Duration::from_millis(300),
        );
        assert_eq!(out, [Some(0), None, Some(10), None]);
        let took = start.elapsed();
        assert!(
            took < Duration::from_millis(1500),
            "waited one deadline, not per job: {took:?}"
        );
    }
}

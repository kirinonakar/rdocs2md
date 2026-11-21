//! Ordered batch execution.
//!
//! Document work is CPU-bound and independent per input, so batches run on
//! several workers while results are still reported in input order.
use std::{
    collections::BTreeMap,
    sync::{Mutex, mpsc},
};

pub const MAX_JOBS: usize = 64;

/// Resolve a requested worker count, falling back to the detected CPU count.
pub fn jobs(requested: Option<usize>) -> usize {
    requested
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
        .clamp(1, MAX_JOBS)
}

/// Default cap on the input bytes converted at the same time.
pub const DEFAULT_INFLIGHT_BYTES: u64 = 1 << 30;

/// Lower a worker count so that the largest worker-sized set of inputs fits
/// `max_bytes`.
///
/// `sizes` are input lengths, so the result bounds the bytes read at once, not
/// the whole working set: parsing and rendering need more memory per input. A
/// zero budget disables the cap.
pub fn jobs_within(requested: Option<usize>, sizes: &[u64], max_bytes: u64) -> usize {
    let jobs = jobs(requested);
    if max_bytes == 0 {
        return jobs;
    }
    let mut sorted = sizes.to_vec();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    let mut total = 0u64;
    let mut fitting = 0usize;
    for size in sorted {
        total = total.saturating_add(size);
        if total > max_bytes {
            break;
        }
        fitting += 1;
    }
    // A single input that exceeds the budget still has to run.
    jobs.min(fitting.max(1))
}

/// Run `work` for each item and hand results to `sink` in input order.
///
/// At most one task per worker stays in flight, so completed results held back
/// for ordering are bounded by the worker count instead of the batch size.
/// `work` must not unwind; callers wrap fallible work in `recover_document`.
pub fn ordered<T, R, F, S>(items: &[T], jobs: usize, work: F, mut sink: S)
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
    S: FnMut(R),
{
    if items.is_empty() {
        return;
    }
    let workers = jobs.clamp(1, items.len());
    if workers == 1 {
        for item in items {
            sink(work(item));
        }
        return;
    }
    // A worker stops on this index, because the queue outlives the scope.
    let stop = items.len();
    let (queue_tx, queue_rx) = mpsc::channel::<usize>();
    let (done_tx, done_rx) = mpsc::channel::<(usize, R)>();
    let queue_rx = Mutex::new(queue_rx);
    let work = &work;
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let queue_rx = &queue_rx;
            let done_tx = done_tx.clone();
            scope.spawn(move || {
                loop {
                    let index = queue_rx
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .recv();
                    let Ok(index) = index else { break };
                    if index == stop || done_tx.send((index, work(&items[index]))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(done_tx);
        let mut pending = BTreeMap::new();
        let (mut sent, mut reported) = (0usize, 0usize);
        'feed: while reported < items.len() {
            while sent < items.len() && sent - reported < workers {
                if queue_tx.send(sent).is_err() {
                    break 'feed;
                }
                sent += 1;
            }
            let Ok((index, result)) = done_rx.recv() else {
                break 'feed;
            };
            pending.insert(index, result);
            while let Some(result) = pending.remove(&reported) {
                sink(result);
                reported += 1;
            }
        }
        for _ in 0..workers {
            let _ = queue_tx.send(stop);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    #[test]
    fn reports_results_in_input_order() {
        let items: Vec<usize> = (0..128).collect();
        let mut seen = Vec::new();
        ordered(&items, 8, |i| i * 2, |v| seen.push(v));
        assert_eq!(seen, items.iter().map(|i| i * 2).collect::<Vec<_>>());
    }

    #[test]
    fn keeps_order_when_completion_reverses() {
        let items: Vec<usize> = (0..16).collect();
        let mut seen = Vec::new();
        ordered(
            &items,
            8,
            |i| {
                std::thread::sleep(Duration::from_millis((16 - i) as u64));
                *i
            },
            |v| seen.push(v),
        );
        assert_eq!(seen, items);
    }

    #[test]
    fn runs_every_item_once() {
        let items: Vec<usize> = (0..300).collect();
        let calls = AtomicUsize::new(0);
        let mut reported = 0;
        ordered(
            &items,
            4,
            |_| calls.fetch_add(1, Ordering::Relaxed),
            |_| reported += 1,
        );
        assert_eq!(calls.load(Ordering::Relaxed), items.len());
        assert_eq!(reported, items.len());
    }

    #[test]
    fn handles_short_and_empty_batches() {
        let mut seen = Vec::new();
        ordered(&[], 8, |i: &usize| *i, |v| seen.push(v));
        assert!(seen.is_empty());
        ordered(&[7], 8, |i| i + 1, |v| seen.push(v));
        assert_eq!(seen, vec![8]);
    }

    #[test]
    fn clamps_worker_count() {
        assert_eq!(jobs(Some(0)), 1);
        assert_eq!(jobs(Some(usize::MAX)), MAX_JOBS);
        assert!((1..=MAX_JOBS).contains(&jobs(None)));
    }

    #[test]
    fn caps_workers_by_inflight_bytes() {
        let mib = 1 << 20;
        let sizes = [mib; 8];
        // A zero budget means no cap.
        assert_eq!(jobs_within(Some(8), &sizes, 0), 8);
        assert_eq!(jobs_within(Some(8), &sizes, 2 * mib), 2);
        // One input that exceeds the budget still needs a worker.
        assert_eq!(jobs_within(Some(8), &sizes, 1), 1);
        assert_eq!(jobs_within(Some(8), &[], 1), 1);
        // Never more workers than inputs, even when everything fits.
        assert_eq!(jobs_within(Some(4), &[1, 1, 1], mib), 3);
    }
}

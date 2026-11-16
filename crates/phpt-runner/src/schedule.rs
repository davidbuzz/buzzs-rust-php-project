// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Parallel execution honouring conflict keys (ref: `run-tests.php` `run_all_tests_parallel()`):
//! while a test holding key K runs, no other test holding K may start; tests with the key `all`
//! run alone at the end.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Condvar, Mutex, PoisonError};
use std::thread;

/// A test to schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Test file path.
    pub path: PathBuf,
    /// Conflict keys.
    pub conflicts: Vec<String>,
}

#[derive(Default)]
struct State {
    pending: VecDeque<Job>,
    active: HashMap<String, usize>,
}

impl State {
    /// Take up to `size` jobs, in order, whose conflict keys are not held by a running batch.
    fn take_batch(&mut self, size: usize) -> Vec<Job> {
        let mut batch = Vec::with_capacity(size);
        let mut i = 0;
        while i < self.pending.len() && batch.len() < size {
            let free = self.pending[i]
                .conflicts
                .iter()
                .all(|k| !self.active.contains_key(k));
            match free.then(|| self.pending.remove(i)).flatten() {
                Some(job) => batch.push(job),
                None => i += 1,
            }
        }
        for job in &batch {
            for k in &job.conflicts {
                *self.active.entry(k.clone()).or_default() += 1;
            }
        }
        batch
    }

    fn release(&mut self, batch: &[Job]) {
        for k in batch.iter().flat_map(|j| &j.conflicts) {
            if let Some(n) = self.active.get_mut(k) {
                *n -= 1;
                if *n == 0 {
                    self.active.remove(k);
                }
            }
        }
    }
}

/// Most tests a worker takes at once (run-tests.php uses 4). Consecutive tests of a directory
/// then usually run one after the other in the same worker, which matters for the many php-src
/// tests that share temporary file names with their neighbours.
const MAX_BATCH: usize = 4;

/// Run `jobs` on `workers` threads, calling `on_result` on the calling thread as results arrive
/// (in completion order). Workers take batches of consecutive jobs; while a batch holds a
/// conflict key, no other batch with that key starts.
pub fn run_jobs<R, F, C>(jobs: Vec<Job>, workers: usize, run: F, mut on_result: C)
where
    R: Send,
    F: Fn(&Job) -> R + Sync,
    C: FnMut(R),
{
    let (sequential, parallel): (Vec<Job>, Vec<Job>) = jobs
        .into_iter()
        .partition(|j| j.conflicts.iter().any(|k| k == "all"));
    let workers = workers.clamp(1, parallel.len().max(1));
    let batch_size = parallel.len().div_ceil(workers).clamp(1, MAX_BATCH);

    let state = Mutex::new(State {
        pending: parallel.into(),
        active: HashMap::new(),
    });
    let cond = Condvar::new();
    let (tx, rx) = mpsc::channel::<R>();

    thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (state, cond, run) = (&state, &cond, &run);
            scope.spawn(move || loop {
                let batch = {
                    let mut guard = state.lock().unwrap_or_else(PoisonError::into_inner);
                    loop {
                        if guard.pending.is_empty() {
                            return;
                        }
                        let batch = guard.take_batch(batch_size);
                        if !batch.is_empty() {
                            break batch;
                        }
                        guard = cond.wait(guard).unwrap_or_else(PoisonError::into_inner);
                    }
                };
                let mut receiver_gone = false;
                for job in &batch {
                    receiver_gone = receiver_gone || tx.send(run(job)).is_err();
                }
                state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .release(&batch);
                cond.notify_all();
                if receiver_gone {
                    return;
                }
            });
        }
        drop(tx);
        for result in rx {
            on_result(result);
        }
    });

    for job in &sequential {
        on_result(run(job));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn job(name: &str, keys: &[&str]) -> Job {
        Job {
            path: PathBuf::from(name),
            conflicts: keys.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn runs_everything_and_respects_conflicts() {
        let jobs: Vec<Job> = (0..20)
            .map(|i| job(&format!("t{i}"), if i % 2 == 0 { &["k"] } else { &[] }))
            .chain([job("last", &["all"])])
            .collect();
        let in_k = AtomicUsize::new(0);
        let max_in_k = AtomicUsize::new(0);
        let mut seen = Vec::new();
        run_jobs(
            jobs,
            8,
            |j| {
                let holds = j.conflicts.iter().any(|k| k == "k");
                if holds {
                    let now = in_k.fetch_add(1, Ordering::SeqCst) + 1;
                    max_in_k.fetch_max(now, Ordering::SeqCst);
                }
                thread::sleep(Duration::from_millis(2));
                if holds {
                    in_k.fetch_sub(1, Ordering::SeqCst);
                }
                j.path.clone()
            },
            |p| seen.push(p),
        );
        assert_eq!(seen.len(), 21);
        assert_eq!(seen.last().unwrap(), &PathBuf::from("last"));
        assert_eq!(max_in_k.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn batches_take_consecutive_free_jobs() {
        let mut state = State {
            pending: [
                job("a", &["x"]),
                job("b", &["y"]),
                job("c", &[]),
                job("d", &[]),
            ]
            .into(),
            active: HashMap::from([("y".to_string(), 1)]),
        };
        let batch = state.take_batch(2);
        let names: Vec<_> = batch.iter().map(|j| j.path.clone()).collect();
        assert_eq!(names, [PathBuf::from("a"), PathBuf::from("c")]);
        assert!(state.active.contains_key("x"));
        state.release(&batch);
        assert!(!state.active.contains_key("x"));
        assert_eq!(state.pending.len(), 2);
    }
}

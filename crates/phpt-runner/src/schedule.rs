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
    fn take_runnable(&mut self) -> Option<Job> {
        let idx = self
            .pending
            .iter()
            .position(|j| j.conflicts.iter().all(|k| !self.active.contains_key(k)))?;
        let job = self.pending.remove(idx)?;
        for k in &job.conflicts {
            *self.active.entry(k.clone()).or_default() += 1;
        }
        Some(job)
    }

    fn release(&mut self, job: &Job) {
        for k in &job.conflicts {
            if let Some(n) = self.active.get_mut(k) {
                *n -= 1;
                if *n == 0 {
                    self.active.remove(k);
                }
            }
        }
    }
}

/// Run `jobs` on `workers` threads, calling `on_result` on the calling thread as results arrive
/// (in completion order).
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
                let job = {
                    let mut guard = state.lock().unwrap_or_else(PoisonError::into_inner);
                    loop {
                        if guard.pending.is_empty() {
                            return;
                        }
                        if let Some(job) = guard.take_runnable() {
                            break job;
                        }
                        guard = cond.wait(guard).unwrap_or_else(PoisonError::into_inner);
                    }
                };
                let result = run(&job);
                state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .release(&job);
                cond.notify_all();
                if tx.send(result).is_err() {
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
}

//! DAG-aware, tokio-based task executor.
//!
//! # Concurrency model
//!
//! Each task in the [`Dag`] gets its own async task. A single
//! [`tokio::sync::watch`] channel is created per task; the sender is
//! given to the task's own future, and a clone of the receiver is given
//! to each of its dependents. A task starts by awaiting every
//! dependency's channel to produce an outcome, inspects their statuses,
//! and decides whether to run, skip (upstream failure), or cache-hit.
//!
//! A [`tokio::sync::Semaphore`] throttles how many tasks can be running
//! a shell command simultaneously. The semaphore is acquired only for
//! the actual command-execution critical section, so cache-hit tasks do
//! not block parallelism.
//!
//! # Why not `rayon`?
//!
//! Build tasks are overwhelmingly I/O-bound (spawning shells, waiting on
//! child processes, reading/writing output files). `tokio::process`
//! gives us streaming stdout/stderr handling and clean cancellation
//! semantics; `rayon` would force us to block worker threads on child
//! processes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;

use crate::cache::Cache;
use crate::dag::Dag;
use crate::error::ExecError;
use crate::shell;
use crate::task::Task;

/// Status of a single task after the run completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    /// Task produced a fresh, successful result.
    Success,
    /// Task was skipped because the cache already held a valid result.
    Cached,
    /// Task's shell command returned a non-zero exit status or failed to spawn.
    Failed,
    /// Task did not run because an upstream dependency failed.
    Skipped,
}

impl TaskStatus {
    /// Returns `true` if the status blocks dependents from running.
    pub fn is_blocking_failure(&self) -> bool {
        matches!(self, Self::Failed | Self::Skipped)
    }
}

/// The outcome of a single task after the executor finishes it.
#[derive(Debug, Clone)]
pub struct TaskOutcome {
    /// Task name.
    pub name: String,
    /// Terminal status.
    pub status: TaskStatus,
    /// Cache key derived for this task (hex SHA-256).
    pub cache_key: Option<String>,
    /// Shell exit code, when the task actually ran.
    pub exit_code: Option<i32>,
    /// Wall-clock time between "queued" and "resolved". For cache hits
    /// this is dominated by input hashing; for skipped tasks it is ~0.
    pub duration: Duration,
    /// Human-readable error message, if any.
    pub error: Option<String>,
}

/// Aggregate report returned at the end of an executor run.
#[derive(Debug, Clone)]
pub struct RunReport {
    /// Per-task outcomes in dependency order.
    pub outcomes: Vec<TaskOutcome>,
}

impl RunReport {
    /// `true` if any task finished with a blocking failure.
    pub fn had_failure(&self) -> bool {
        self.outcomes.iter().any(|o| o.status.is_blocking_failure())
    }

    /// Look up the outcome for a specific task.
    pub fn get(&self, name: &str) -> Option<&TaskOutcome> {
        self.outcomes.iter().find(|o| o.name == name)
    }
}

/// Events emitted by the executor while a run is in progress.
#[derive(Debug, Clone)]
pub enum ExecutionEvent {
    /// Task started evaluating (dependencies done, acquiring cache/semaphore).
    Started {
        /// Task name.
        task: String,
    },
    /// Task resulted in a cache hit and will not run its command.
    Cached {
        /// Task name.
        task: String,
        /// Cache key (hex).
        key: String,
    },
    /// Task finished successfully.
    Finished {
        /// Task name.
        task: String,
        /// Elapsed wall-clock time.
        duration: Duration,
    },
    /// Task failed to run (non-zero exit or spawn failure).
    Failed {
        /// Task name.
        task: String,
        /// Exit code when one was produced.
        exit_code: Option<i32>,
        /// Human-readable error message.
        message: String,
    },
    /// Task was skipped because an upstream dep failed.
    Skipped {
        /// Task name.
        task: String,
        /// Upstream that caused the skip.
        upstream: String,
    },
    /// A single line of the task's stdout.
    Stdout {
        /// Task name.
        task: String,
        /// Captured line (without the trailing newline).
        line: String,
    },
    /// A single line of the task's stderr.
    Stderr {
        /// Task name.
        task: String,
        /// Captured line (without the trailing newline).
        line: String,
    },
}

/// Callback invoked for every [`ExecutionEvent`] emitted by the executor.
pub type Reporter = Arc<dyn Fn(ExecutionEvent) + Send + Sync>;

/// Configuration knobs for [`run`].
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Maximum number of tasks that may run their shell command concurrently.
    pub concurrency: usize,
    /// Project root — commands are invoked with this as their working directory.
    pub project_root: PathBuf,
    /// When `false`, every task is treated as a cache miss.
    pub use_cache: bool,
}

impl RunOptions {
    /// Construct sensible defaults from a project root and concurrency.
    pub fn new(project_root: impl Into<PathBuf>, concurrency: usize) -> Self {
        Self {
            project_root: project_root.into(),
            concurrency: concurrency.max(1),
            use_cache: true,
        }
    }
}

/// Compute the transitive dependency closure of `targets` within `dag`.
///
/// If `targets` is empty, returns every task.
pub fn select_tasks(dag: &Dag, targets: &[String]) -> Result<Vec<Task>, ExecError> {
    if targets.is_empty() {
        return Ok(dag.tasks().to_vec());
    }
    let all: BTreeMap<&str, &Task> = dag.tasks().iter().map(|t| (t.name.as_str(), t)).collect();

    for t in targets {
        if !all.contains_key(t.as_str()) {
            return Err(ExecError::Io {
                task: t.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no such task `{t}`"),
                ),
            });
        }
    }

    let mut selected: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<String> = targets.to_vec();
    while let Some(n) = stack.pop() {
        if !selected.insert(n.clone()) {
            continue;
        }
        for d in dag.deps_of(&n) {
            stack.push(d.clone());
        }
    }
    // Preserve dag-declaration order for determinism.
    Ok(dag
        .tasks()
        .iter()
        .filter(|t| selected.contains(&t.name))
        .cloned()
        .collect())
}

/// Run `tasks` in parallel honoring their dependency order.
///
/// The ordering is enforced by per-task [`watch`] channels; cancellation
/// of dependents on failure is enforced by dependents observing a
/// `Failed`/`Skipped` outcome and transitioning to `Skipped` themselves.
pub async fn run(
    tasks: &[Task],
    cache: &Cache,
    options: &RunOptions,
    reporter: Option<Reporter>,
) -> Result<RunReport, ExecError> {
    // Sub-DAG just for `tasks` (so `deps_of` only includes selected tasks).
    let dag = Dag::build(tasks).map_err(|e| ExecError::Io {
        task: "<plan>".into(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()),
    })?;

    let sem = Arc::new(Semaphore::new(options.concurrency.max(1)));
    let cache = Arc::new(cache.clone());
    let opts = Arc::new(options.clone());
    let reporter = reporter.unwrap_or_else(|| Arc::new(|_| {}));

    // One watch channel per task.
    let mut senders: HashMap<String, watch::Sender<Option<TaskOutcome>>> = HashMap::new();
    let mut receivers: HashMap<String, watch::Receiver<Option<TaskOutcome>>> = HashMap::new();
    for name in dag.topo_order() {
        let (tx, rx) = watch::channel(None);
        senders.insert(name.clone(), tx);
        receivers.insert(name.clone(), rx);
    }

    let mut join_set: JoinSet<TaskOutcome> = JoinSet::new();
    for task in dag.tasks().iter().cloned() {
        let dep_receivers: Vec<watch::Receiver<Option<TaskOutcome>>> = dag
            .deps_of(&task.name)
            .iter()
            .map(|d| receivers[d].clone())
            .collect();
        let my_sender = senders.remove(&task.name).expect("sender prepared");
        let sem = Arc::clone(&sem);
        let cache = Arc::clone(&cache);
        let opts = Arc::clone(&opts);
        let reporter = Arc::clone(&reporter);

        join_set.spawn(async move {
            let outcome = run_single(task, dep_receivers, sem, cache, opts, reporter).await;
            // Publish for dependents. Ignoring the send error is fine:
            // dependents that dropped their receiver cannot be waiting.
            let _ = my_sender.send(Some(outcome.clone()));
            outcome
        });
    }

    let mut outcomes: HashMap<String, TaskOutcome> = HashMap::new();
    while let Some(joined) = join_set.join_next().await {
        let outcome = joined.map_err(|e| ExecError::Io {
            task: "<runtime>".into(),
            source: std::io::Error::other(e.to_string()),
        })?;
        outcomes.insert(outcome.name.clone(), outcome);
    }

    let ordered = dag
        .topo_order()
        .iter()
        .filter_map(|n| outcomes.remove(n))
        .collect();

    Ok(RunReport { outcomes: ordered })
}

async fn run_single(
    task: Task,
    mut dep_receivers: Vec<watch::Receiver<Option<TaskOutcome>>>,
    sem: Arc<Semaphore>,
    cache: Arc<Cache>,
    opts: Arc<RunOptions>,
    reporter: Reporter,
) -> TaskOutcome {
    let started = Instant::now();

    // Wait for every dep. Collect their cache keys (for our own key
    // derivation) and detect upstream failures.
    let mut dep_keys: Vec<String> = Vec::with_capacity(dep_receivers.len());
    let mut blocking_upstream: Option<String> = None;
    for rx in dep_receivers.iter_mut() {
        // Fast path: the value may already be present.
        if rx.borrow().is_none() {
            // Await change. If the sender was dropped without sending,
            // treat as an internal failure.
            if rx.changed().await.is_err() {
                return TaskOutcome {
                    name: task.name.clone(),
                    status: TaskStatus::Skipped,
                    cache_key: None,
                    exit_code: None,
                    duration: started.elapsed(),
                    error: Some("upstream dep channel closed".into()),
                };
            }
        }
        let outcome = rx
            .borrow()
            .clone()
            .expect("dep channel yielded None after change");
        if outcome.status.is_blocking_failure() {
            blocking_upstream = Some(outcome.name.clone());
            break;
        }
        if let Some(k) = outcome.cache_key {
            dep_keys.push(k);
        }
    }

    if let Some(upstream) = blocking_upstream {
        reporter(ExecutionEvent::Skipped {
            task: task.name.clone(),
            upstream: upstream.clone(),
        });
        return TaskOutcome {
            name: task.name.clone(),
            status: TaskStatus::Skipped,
            cache_key: None,
            exit_code: None,
            duration: started.elapsed(),
            error: Some(format!("upstream `{upstream}` failed")),
        };
    }

    // Compute the cache key. If this fails (e.g. broken glob or missing
    // input file), treat it as a task failure with a clear message.
    let cache_key = match cache.compute_key(&task, &dep_keys) {
        Ok(k) => k,
        Err(e) => {
            let msg = format!("cache key derivation failed: {e}");
            reporter(ExecutionEvent::Failed {
                task: task.name.clone(),
                exit_code: None,
                message: msg.clone(),
            });
            return TaskOutcome {
                name: task.name.clone(),
                status: TaskStatus::Failed,
                cache_key: None,
                exit_code: None,
                duration: started.elapsed(),
                error: Some(msg),
            };
        }
    };

    // Cache hit?
    if opts.use_cache {
        match cache.lookup(&cache_key) {
            Ok(Some(_hit)) => {
                reporter(ExecutionEvent::Cached {
                    task: task.name.clone(),
                    key: cache_key.clone(),
                });
                return TaskOutcome {
                    name: task.name.clone(),
                    status: TaskStatus::Cached,
                    cache_key: Some(cache_key),
                    exit_code: Some(0),
                    duration: started.elapsed(),
                    error: None,
                };
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(task = %task.name, error = %e, "cache lookup failed; treating as miss");
            }
        }
    }

    reporter(ExecutionEvent::Started {
        task: task.name.clone(),
    });

    // Acquire the concurrency slot only for the duration of the shell command.
    let _permit = match sem.acquire().await {
        Ok(p) => p,
        Err(_) => {
            let msg = "semaphore closed".to_string();
            reporter(ExecutionEvent::Failed {
                task: task.name.clone(),
                exit_code: None,
                message: msg.clone(),
            });
            return TaskOutcome {
                name: task.name.clone(),
                status: TaskStatus::Failed,
                cache_key: Some(cache_key),
                exit_code: None,
                duration: started.elapsed(),
                error: Some(msg),
            };
        }
    };

    let span = tracing::info_span!("task", name = %task.name);
    let _enter = span.enter();

    let run_result = run_command(&task, &opts.project_root, &reporter).await;
    drop(_enter);

    match run_result {
        Ok(0) => {
            if opts.use_cache {
                if let Err(e) = cache.store(&task, &cache_key, 0) {
                    tracing::warn!(task = %task.name, error = %e, "cache store failed");
                }
            }
            let duration = started.elapsed();
            reporter(ExecutionEvent::Finished {
                task: task.name.clone(),
                duration,
            });
            TaskOutcome {
                name: task.name,
                status: TaskStatus::Success,
                cache_key: Some(cache_key),
                exit_code: Some(0),
                duration,
                error: None,
            }
        }
        Ok(code) => {
            let msg = format!("exit code {code}");
            reporter(ExecutionEvent::Failed {
                task: task.name.clone(),
                exit_code: Some(code),
                message: msg.clone(),
            });
            TaskOutcome {
                name: task.name,
                status: TaskStatus::Failed,
                cache_key: Some(cache_key),
                exit_code: Some(code),
                duration: started.elapsed(),
                error: Some(msg),
            }
        }
        Err(e) => {
            let msg = e.to_string();
            reporter(ExecutionEvent::Failed {
                task: task.name.clone(),
                exit_code: None,
                message: msg.clone(),
            });
            TaskOutcome {
                name: task.name,
                status: TaskStatus::Failed,
                cache_key: Some(cache_key),
                exit_code: None,
                duration: started.elapsed(),
                error: Some(msg),
            }
        }
    }
}

async fn run_command(task: &Task, cwd: &Path, reporter: &Reporter) -> Result<i32, std::io::Error> {
    use tokio::process::Command;
    let invocation = shell::resolve();
    let mut cmd = Command::new(&invocation.program);
    cmd.args(&invocation.args)
        .arg(&task.command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in &task.env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn()?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_task = stdout.map(|s| {
        let name = task.name.clone();
        let reporter = Arc::clone(reporter);
        tokio::spawn(async move {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                reporter(ExecutionEvent::Stdout {
                    task: name.clone(),
                    line,
                });
            }
        })
    });
    let stderr_task = stderr.map(|s| {
        let name = task.name.clone();
        let reporter = Arc::clone(reporter);
        tokio::spawn(async move {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                reporter(ExecutionEvent::Stderr {
                    task: name.clone(),
                    line,
                });
            }
        })
    });

    let status = child.wait().await?;

    if let Some(h) = stdout_task {
        let _ = h.await;
    }
    if let Some(h) = stderr_task {
        let _ = h.await;
    }

    Ok(status.code().unwrap_or(-1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    // Cross-platform test command helpers. The default shell is `sh -c`
    // on Unix and PowerShell on Windows, so test commands must be
    // expressed in whichever dialect matches the host.

    #[cfg(windows)]
    fn sleep_cmd(ms: u64) -> String {
        format!("Start-Sleep -Milliseconds {ms}")
    }
    #[cfg(not(windows))]
    fn sleep_cmd(ms: u64) -> String {
        format!("sleep {}", ms as f64 / 1000.0)
    }

    #[cfg(windows)]
    fn fail_cmd() -> String {
        "exit 1".to_string()
    }
    #[cfg(not(windows))]
    fn fail_cmd() -> String {
        "false".to_string()
    }

    /// Write `body` to `path` (relative to the task's cwd) using a
    /// shell-native command. PowerShell 5.1's `>` redirects emit
    /// UTF-16 + BOM; we use `Set-Content -Encoding ascii` instead so
    /// the bytes match what `echo > file` produces under `sh`.
    #[cfg(windows)]
    fn write_cmd(path: &str, body: &str) -> String {
        format!("Set-Content -Encoding ascii -Path '{path}' -Value '{body}'")
    }
    #[cfg(not(windows))]
    fn write_cmd(path: &str, body: &str) -> String {
        format!("echo {body} > {path}")
    }

    fn mk(name: &str, cmd: &str, deps: &[&str]) -> Task {
        Task {
            name: name.into(),
            command: cmd.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            inputs: vec![],
            outputs: vec![],
            env: BTreeMap::new(),
        }
    }

    fn events_collector() -> (Reporter, Arc<Mutex<Vec<ExecutionEvent>>>) {
        let store: Arc<Mutex<Vec<ExecutionEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let store_clone = Arc::clone(&store);
        let r: Reporter = Arc::new(move |e: ExecutionEvent| {
            store_clone.lock().unwrap().push(e);
        });
        (r, store)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn runs_independent_tasks_concurrently() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::open(tmp.path()).unwrap();
        // PowerShell's per-invocation startup is ~0.5–1s on a fresh CI
        // runner, so we need (a) a sleep large enough that the signal
        // dominates spawn noise and (b) a threshold sized as
        // `sleep + spawn_budget` rather than a tight multiple of the
        // sleep, so parallel runs sit well under it while serial runs
        // (which pay startup twice) sit comfortably above.
        let (sleep_ms, threshold_ms): (u64, u64) = if cfg!(windows) {
            (2000, 4500)
        } else {
            (300, 550)
        };
        let tasks = vec![
            mk("a", &sleep_cmd(sleep_ms), &[]),
            mk("b", &sleep_cmd(sleep_ms), &[]),
        ];
        let opts = RunOptions::new(tmp.path(), 4);
        let started = Instant::now();
        let report = run(&tasks, &cache, &opts, None).await.unwrap();
        let elapsed = started.elapsed();
        assert!(!report.had_failure());
        assert!(
            elapsed < Duration::from_millis(threshold_ms),
            "elapsed={elapsed:?} — tasks appear to have run serially"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failure_skips_dependents() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::open(tmp.path()).unwrap();
        let tasks = vec![
            mk("a", &fail_cmd(), &[]),
            mk("b", "echo should-not-run", &["a"]),
        ];
        let opts = RunOptions::new(tmp.path(), 2);
        let report = run(&tasks, &cache, &opts, None).await.unwrap();
        assert_eq!(report.get("a").unwrap().status, TaskStatus::Failed);
        assert_eq!(report.get("b").unwrap().status, TaskStatus::Skipped);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn second_run_hits_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = Cache::open(tmp.path()).unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        // Intentionally write deterministic output so the cache can verify it.
        let cmd = write_cmd("out.txt", "hi");
        let task = Task {
            name: "t".into(),
            command: cmd,
            deps: vec![],
            inputs: vec![],
            outputs: vec!["out.txt".into()],
            env: BTreeMap::new(),
        };
        let opts = RunOptions::new(tmp.path(), 1);

        let c = Arc::clone(&counter);
        let reporter: Reporter = Arc::new(move |e| {
            if matches!(e, ExecutionEvent::Finished { .. }) {
                c.fetch_add(1, Ordering::SeqCst);
            }
        });

        let r1 = run(
            std::slice::from_ref(&task),
            &cache,
            &opts,
            Some(reporter.clone()),
        )
        .await
        .unwrap();
        assert_eq!(r1.get("t").unwrap().status, TaskStatus::Success);

        let (rep2, store) = events_collector();
        let r2 = run(&[task], &cache, &opts, Some(rep2)).await.unwrap();
        assert_eq!(r2.get("t").unwrap().status, TaskStatus::Cached);
        let events = store.lock().unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, ExecutionEvent::Cached { .. })));
    }
}

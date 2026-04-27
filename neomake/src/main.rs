//! `neomake` CLI binary.
//!
//! This is a thin wrapper that:
//!
//! 1. Parses command-line arguments with [`cli`].
//! 2. Locates the config file (`--config`, else `neomake.toml`, else `neomake.tomlx`).
//! 3. Loads it through `neomake-tomlx` (graceful TOML degradation).
//! 4. Hands the resulting [`neomake_core::TaskSet`] to the DAG
//!    executor.
//! 5. Prints human-readable status events as the run progresses.

mod cli;

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use clap::Parser;

use neomake_core::{
    executor, Cache, Dag, ExecutionEvent, Reporter, RunOptions, TaskSet, TaskStatus,
};

use crate::cli::{CacheCommand, Cli, Command};

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(err) => {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "error: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn real_main() -> Result<ExitCode> {
    init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Command::Run { ref tasks } => {
            let config = resolve_config(cli.config.as_deref())?;
            let value = neomake_tomlx::load(&config)
                .with_context(|| format!("failed to load `{}`", config.display()))?;
            let taskset = TaskSet::from_value(value, &config)
                .with_context(|| format!("invalid configuration in `{}`", config.display()))?;
            let dag = Dag::build(taskset.tasks())?;
            let selected = executor::select_tasks(&dag, tasks)?;
            let project_root = neomake_core::project_root_for(&config);
            let cache = Cache::open(&project_root)?;
            let concurrency = cli.concurrency.unwrap_or_else(num_cpus::get);

            let options = RunOptions {
                project_root,
                concurrency,
                use_cache: !cli.no_cache,
            };

            let quiet = cli.quiet;
            let color = io::stdout().is_terminal();
            let reporter = make_reporter(color, quiet);

            let report = tokio_runtime()?.block_on(executor::run(
                &selected,
                &cache,
                &options,
                Some(reporter),
            ))?;

            print_summary(&report);
            if report.had_failure() {
                Ok(ExitCode::from(1))
            } else {
                Ok(ExitCode::from(0))
            }
        }
        Command::List => {
            let config = resolve_config(cli.config.as_deref())?;
            let value = neomake_tomlx::load(&config)?;
            let taskset = TaskSet::from_value(value, &config)?;
            let dag = Dag::build(taskset.tasks())?;
            println!("tasks in `{}` (topological order):", config.display());
            for name in dag.topo_order() {
                let deps = dag.deps_of(name);
                if deps.is_empty() {
                    println!("  {name}");
                } else {
                    println!("  {name}  (deps: {})", deps.join(", "));
                }
            }
            Ok(ExitCode::from(0))
        }
        Command::Cache(cmd) => {
            let config = resolve_config(cli.config.as_deref()).ok();
            let project_root = config
                .as_deref()
                .map(neomake_core::project_root_for)
                .unwrap_or_else(|| PathBuf::from("."));
            let cache = Cache::open(&project_root)?;
            match cmd {
                CacheCommand::Clean => {
                    cache.clean()?;
                    println!("cache cleaned: {}", cache.dir().display());
                }
                CacheCommand::Status => {
                    let status = cache.status()?;
                    println!(
                        "cache at {}: {} entries, {} bytes",
                        cache.dir().display(),
                        status.entries,
                        status.bytes
                    );
                }
            }
            Ok(ExitCode::from(0))
        }
    }
}

fn resolve_config(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if !p.exists() {
            return Err(anyhow!("config file not found: {}", p.display()));
        }
        return Ok(p.to_path_buf());
    }
    let candidates = ["neomake.toml", "neomake.tomlx"];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(anyhow!(
        "no configuration file found (looked for `neomake.toml` and `neomake.tomlx`)"
    ))
}

fn tokio_runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start tokio runtime")
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("NEOMAKE_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .try_init();
}

fn make_reporter(color: bool, quiet: bool) -> Reporter {
    let stdout = Arc::new(Mutex::new(io::stdout()));
    let c = stdout.clone();
    Arc::new(move |event: ExecutionEvent| {
        let mut out = c.lock().unwrap_or_else(|e| e.into_inner());
        match event {
            ExecutionEvent::Started { task } => {
                let tag = colorize(color, "cyan", "[RUNNING]");
                let _ = writeln!(out, "{tag} {task}");
            }
            ExecutionEvent::Cached { task, key } => {
                let short: String = key.chars().take(12).collect();
                let tag = colorize(color, "yellow", "[CACHED ]");
                let _ = writeln!(out, "{tag} {task}  (key={short}…)");
            }
            ExecutionEvent::Finished { task, duration } => {
                let tag = colorize(color, "green", "[OK     ]");
                let _ = writeln!(out, "{tag} {task}  ({:.2}s)", duration.as_secs_f64());
            }
            ExecutionEvent::Failed {
                task,
                exit_code,
                message,
            } => {
                let tag = colorize(color, "red", "[FAILED ]");
                let code = match exit_code {
                    Some(c) => format!(" (exit={c})"),
                    None => String::new(),
                };
                let _ = writeln!(out, "{tag} {task}{code}  {message}");
            }
            ExecutionEvent::Skipped { task, upstream } => {
                let tag = colorize(color, "magenta", "[SKIPPED]");
                let _ = writeln!(out, "{tag} {task}  (upstream `{upstream}` failed)");
            }
            ExecutionEvent::Stdout { task, line } => {
                if !quiet {
                    let _ = writeln!(out, "    {task} | {line}");
                }
            }
            ExecutionEvent::Stderr { task, line } => {
                if !quiet {
                    let _ = writeln!(out, "    {task} ! {line}");
                }
            }
        }
    })
}

fn colorize(enabled: bool, color: &str, text: &str) -> String {
    if !enabled {
        return text.to_string();
    }
    let code = match color {
        "red" => "31",
        "green" => "32",
        "yellow" => "33",
        "cyan" => "36",
        "magenta" => "35",
        _ => return text.to_string(),
    };
    format!("\x1b[{code}m{text}\x1b[0m")
}

fn print_summary(report: &neomake_core::RunReport) {
    let (mut ok, mut cached, mut failed, mut skipped) = (0, 0, 0, 0);
    for o in &report.outcomes {
        match o.status {
            TaskStatus::Success => ok += 1,
            TaskStatus::Cached => cached += 1,
            TaskStatus::Failed => failed += 1,
            TaskStatus::Skipped => skipped += 1,
        }
    }
    println!("summary: {ok} ok, {cached} cached, {failed} failed, {skipped} skipped");
}

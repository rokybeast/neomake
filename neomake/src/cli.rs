//! CLI surface for `neomake`.
//!
//! Parsed with `clap` (derive). The CLI is intentionally small:
//!
//! ```text
//! neomake [--config <FILE>] [--concurrency N] run [TASK]...
//! neomake [--config <FILE>]                       list
//! neomake                                           cache clean
//! neomake                                           cache status
//! ```

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Top-level CLI arguments.
#[derive(Debug, Parser)]
#[command(name = "neomake", version, about = "A TOML/TOMLX build orchestrator.")]
pub struct Cli {
    /// Path to the configuration file. If omitted, `neomake.toml` then
    /// `neomake.tomlx` are tried in the current directory.
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    /// Maximum number of tasks that may run shell commands concurrently.
    #[arg(long, global = true)]
    pub concurrency: Option<usize>,

    /// Disable all cache lookups for this invocation.
    #[arg(long, global = true, default_value_t = false)]
    pub no_cache: bool,

    /// Quiet mode — suppress per-task stdout/stderr forwarding.
    #[arg(short, long, global = true, default_value_t = false)]
    pub quiet: bool,

    /// Subcommand.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run one or more tasks (and their dependencies). If no task name
    /// is given, every task is run.
    Run {
        /// Names of tasks to build.
        #[arg(value_name = "TASK")]
        tasks: Vec<String>,
    },
    /// List every task in topological order with its dependencies.
    List,
    /// Cache management subcommands.
    #[command(subcommand)]
    Cache(CacheCommand),
}

/// `cache` subcommands.
#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Remove every entry from the cache directory.
    Clean,
    /// Print a one-line summary of the cache contents.
    Status,
}

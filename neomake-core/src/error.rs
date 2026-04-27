//! Error types used throughout `neomake-core`.
//!
//! All user-facing error variants carry enough context (source file path,
//! optional line number, and a descriptive message) to produce actionable
//! diagnostics without resorting to `unwrap()` in call sites.

use std::path::PathBuf;

use thiserror::Error;

/// Top-level error returned by `neomake-core` operations.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Failure while reading or validating a configuration file.
    #[error("{0}")]
    Config(#[from] ConfigError),

    /// Failure while constructing or traversing the task DAG.
    #[error("{0}")]
    Dag(#[from] DagError),

    /// Failure while executing a task.
    #[error("{0}")]
    Exec(#[from] ExecError),

    /// Failure while reading from or writing to the cache.
    #[error("{0}")]
    Cache(#[from] CacheError),
}

/// A configuration-file error with file/line context.
#[derive(Debug, Error)]
#[error("{path}{line_suffix}: {message}", line_suffix = line_suffix(*line))]
pub struct ConfigError {
    /// Path to the offending configuration file.
    pub path: PathBuf,
    /// 1-indexed line number, when known.
    pub line: Option<usize>,
    /// Human-readable message describing the problem.
    pub message: String,
}

impl ConfigError {
    /// Construct a new [`ConfigError`].
    pub fn new(path: impl Into<PathBuf>, line: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            line,
            message: message.into(),
        }
    }
}

/// Errors produced while building or validating the task DAG.
#[derive(Debug, Error)]
pub enum DagError {
    /// Detected a dependency cycle. `path` is the concrete cycle, e.g.
    /// `["a", "b", "c", "a"]`.
    #[error("dependency cycle detected: {}", format_cycle(path))]
    Cycle {
        /// Task names forming the cycle, with the first task repeated at the end.
        path: Vec<String>,
    },

    /// A task refers to a dependency that does not exist.
    #[error("task `{task}` depends on unknown task `{missing}`")]
    UnknownDependency {
        /// The task that declared the bad dependency.
        task: String,
        /// The missing dependency name.
        missing: String,
    },
}

/// Errors produced while executing tasks.
#[derive(Debug, Error)]
pub enum ExecError {
    /// The task's command exited with a non-zero status.
    #[error("task `{task}` failed with exit code {code}")]
    TaskFailed {
        /// Name of the failing task.
        task: String,
        /// Exit code returned by the command (or -1 if terminated by signal).
        code: i32,
    },

    /// Task was skipped because an upstream dependency failed.
    #[error("task `{task}` skipped because upstream dependency `{upstream}` failed")]
    UpstreamFailed {
        /// Name of the skipped task.
        task: String,
        /// Name of the upstream task whose failure caused the skip.
        upstream: String,
    },

    /// Transient I/O error encountered while launching a task.
    #[error("task `{task}`: i/o error: {source}")]
    Io {
        /// Name of the task being launched.
        task: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Errors produced by the cache subsystem.
#[derive(Debug, Error)]
pub enum CacheError {
    /// I/O error while accessing the cache directory.
    #[error("cache i/o error at {path}: {source}")]
    Io {
        /// Path of the file or directory involved.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A cache entry file was malformed.
    #[error("corrupt cache entry at {path}: {message}")]
    Corrupt {
        /// Path to the malformed entry.
        path: PathBuf,
        /// Description of the corruption.
        message: String,
    },
}

fn line_suffix(line: Option<usize>) -> String {
    match line {
        Some(n) => format!(":{n}"),
        None => String::new(),
    }
}

fn format_cycle(path: &[String]) -> String {
    path.join(" -> ")
}

//! Task model and TOML deserialization.
//!
//! A [`Task`] is the atomic unit scheduled by the DAG executor. A
//! [`TaskSet`] is the collection of tasks loaded from a configuration
//! file; it performs structural validation (unknown-key rejection,
//! duplicate detection) and preserves a stable task ordering.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::ConfigError;

/// A single build task.
///
/// `inputs` and `outputs` are glob patterns, resolved relative to the
/// project root when the DAG is executed. `env` uses [`BTreeMap`] so its
/// iteration order is deterministic, which matters for cache-key stability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Unique task name (the table key in `[tasks.<name>]`).
    pub name: String,
    /// Shell command to execute.
    pub command: String,
    /// Names of tasks this task depends on.
    pub deps: Vec<String>,
    /// Glob patterns selecting files that are inputs to the task.
    pub inputs: Vec<String>,
    /// Glob patterns selecting files produced by the task.
    pub outputs: Vec<String>,
    /// Extra environment variables set when running the task.
    pub env: BTreeMap<String, String>,
}

/// A collection of tasks loaded from a configuration file.
#[derive(Debug, Clone, Default)]
pub struct TaskSet {
    tasks: Vec<Task>,
}

impl TaskSet {
    /// Access all tasks in configuration order.
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// Look up a task by name.
    pub fn get(&self, name: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.name == name)
    }

    /// Load a [`TaskSet`] from a pre-parsed [`toml::Value`].
    ///
    /// `source_path` is used purely for error reporting and may point to
    /// either a `.toml` or `.tomlx` file.
    pub fn from_value(value: toml::Value, source_path: &Path) -> Result<Self, ConfigError> {
        let raw: RawConfig = value.try_into().map_err(|e| ConfigError {
            path: source_path.to_path_buf(),
            line: None,
            message: format!("invalid configuration schema: {e}"),
        })?;

        let mut tasks = Vec::with_capacity(raw.tasks.len());
        let mut seen = BTreeSet::new();
        let mut ordered: Vec<(String, RawTask)> = raw.tasks.into_iter().collect();
        ordered.sort_by(|a, b| a.0.cmp(&b.0));

        for (name, raw_task) in ordered {
            if !seen.insert(name.clone()) {
                return Err(ConfigError::new(
                    source_path,
                    None,
                    format!("duplicate task definition: `{name}`"),
                ));
            }
            if name.is_empty() {
                return Err(ConfigError::new(
                    source_path,
                    None,
                    "task name cannot be empty",
                ));
            }
            if raw_task.command.trim().is_empty() {
                return Err(ConfigError::new(
                    source_path,
                    None,
                    format!("task `{name}` has an empty `command`"),
                ));
            }
            tasks.push(Task {
                name,
                command: raw_task.command,
                deps: raw_task.deps,
                inputs: raw_task.inputs,
                outputs: raw_task.outputs,
                env: raw_task.env,
            });
        }

        Ok(Self { tasks })
    }

    /// Load a [`TaskSet`] directly from a plain `.toml` file on disk.
    ///
    /// This is a convenience wrapper that does not understand TOMLX.
    /// Binaries should prefer going through `neomake-tomlx`, which
    /// gracefully degrades to plain TOML when no extended syntax is used.
    pub fn from_toml_file(path: &Path) -> Result<Self, ConfigError> {
        let source = std::fs::read_to_string(path).map_err(|e| ConfigError {
            path: path.to_path_buf(),
            line: None,
            message: format!("cannot read config: {e}"),
        })?;
        let value: toml::Value = toml::from_str(&source).map_err(|e| {
            let line = e
                .span()
                .and_then(|range| line_from_offset(&source, range.start));
            ConfigError {
                path: path.to_path_buf(),
                line,
                message: format!("toml parse error: {}", e.message()),
            }
        })?;
        Self::from_value(value, path)
    }
}

/// Locate the project root for a given configuration file path.
///
/// The project root is simply the directory containing the config; this
/// is where `.neomake/cache/` and all glob patterns are resolved.
pub fn project_root_for(config_path: &Path) -> PathBuf {
    match config_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn line_from_offset(source: &str, offset: usize) -> Option<usize> {
    if offset > source.len() {
        return None;
    }
    Some(source[..offset].bytes().filter(|&b| b == b'\n').count() + 1)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    tasks: BTreeMap<String, RawTask>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTask {
    command: String,
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    outputs: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn parse(source: &str) -> Result<TaskSet, ConfigError> {
        let value: toml::Value = toml::from_str(source).unwrap();
        TaskSet::from_value(value, &PathBuf::from("test.toml"))
    }

    #[test]
    fn parses_minimal_task() {
        let set = parse(
            r#"
            [tasks.build]
            command = "cargo build"
            "#,
        )
        .unwrap();
        assert_eq!(set.tasks().len(), 1);
        let t = set.get("build").unwrap();
        assert_eq!(t.command, "cargo build");
        assert!(t.deps.is_empty());
    }

    #[test]
    fn rejects_unknown_task_field() {
        let err = parse(
            r#"
            [tasks.build]
            command = "x"
            mystery = "?"
            "#,
        )
        .unwrap_err();
        assert!(
            err.message.contains("unknown field"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn rejects_empty_command() {
        let err = parse(
            r#"
            [tasks.build]
            command = ""
            "#,
        )
        .unwrap_err();
        assert!(err.message.contains("empty `command`"));
    }

    #[test]
    fn preserves_env_and_deps() {
        let set = parse(
            r#"
            [tasks.a]
            command = "echo a"

            [tasks.b]
            command = "echo b"
            deps = ["a"]
            inputs = ["src/**/*.rs"]
            outputs = ["out.txt"]
            env = { FOO = "bar", BAZ = "qux" }
            "#,
        )
        .unwrap();
        let b = set.get("b").unwrap();
        assert_eq!(b.deps, vec!["a".to_string()]);
        assert_eq!(b.env.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(b.env.get("BAZ"), Some(&"qux".to_string()));
    }
}

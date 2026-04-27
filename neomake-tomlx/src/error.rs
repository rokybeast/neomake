//! Error type for TOMLX parsing and evaluation.

use std::path::PathBuf;

use thiserror::Error;

/// A diagnostic reporting a TOMLX failure with file/line/col context.
#[derive(Debug, Error, Clone)]
#[error("{file}:{line}:{col}: {message}", file = file.display())]
pub struct TomlxError {
    /// Path of the source file (real or synthetic).
    pub file: PathBuf,
    /// 1-indexed line number.
    pub line: usize,
    /// 1-indexed column number.
    pub col: usize,
    /// Human-readable description of the problem.
    pub message: String,
}

impl TomlxError {
    /// Construct a new [`TomlxError`].
    pub fn new(
        file: impl Into<PathBuf>,
        line: usize,
        col: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            file: file.into(),
            line,
            col,
            message: message.into(),
        }
    }

    /// Construct an error whose location is unknown.
    pub fn at_unknown(file: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            line: 0,
            col: 0,
            message: message.into(),
        }
    }
}

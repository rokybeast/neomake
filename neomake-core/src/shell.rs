//! Platform-aware shell resolution.
//!
//! Both the task executor (`executor::run_command`) and the TOMLX
//! `exec()` built-in need to launch user-authored command strings via a
//! shell. The choice of shell is platform-dependent and overridable.
//!
//! # Resolution order
//!
//! 1. If the `NEOMAKE_SHELL` environment variable is set, its value is
//!    whitespace-tokenized as an argv. The first token is the program;
//!    subsequent tokens are passed as leading arguments. The user's
//!    command string is appended as the final argv element. Empty
//!    overrides are ignored.
//! 2. Otherwise, on Unix-likes the default is `sh -c`.
//! 3. On Windows the default is the first that exists on `PATH`:
//!    - `pwsh` (PowerShell 7+, defaults to UTF-8) with
//!      `-NoProfile -NoLogo -Command`
//!    - `powershell` (Windows PowerShell 5.1) with the same flags
//!    - `cmd.exe` with `/S /C` as the last-resort fallback
//!
//! The first call caches the resolved invocation in a [`OnceLock`] so
//! the (potentially expensive) `which` lookup happens once per process.

use std::sync::OnceLock;

/// A fully-resolved shell invocation: the program to spawn plus the
/// argv prefix that precedes the user's command string.
#[derive(Debug, Clone)]
pub struct ShellInvocation {
    /// Executable to spawn (e.g. `sh`, `pwsh`, `cmd`).
    pub program: String,
    /// Arguments inserted before the command string (e.g. `["-c"]`).
    pub args: Vec<String>,
}

impl ShellInvocation {
    fn new(program: impl Into<String>, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }
}

static RESOLVED: OnceLock<ShellInvocation> = OnceLock::new();

/// Return the shell invocation neomake should use, computing and
/// caching it on first access.
///
/// See the module-level docs for the resolution rules.
pub fn resolve() -> ShellInvocation {
    RESOLVED.get_or_init(compute).clone()
}

fn compute() -> ShellInvocation {
    if let Some(inv) = override_from_env() {
        tracing::debug!(
            program = %inv.program,
            args = ?inv.args,
            "using NEOMAKE_SHELL override"
        );
        return inv;
    }
    default_for_platform()
}

fn override_from_env() -> Option<ShellInvocation> {
    let raw = std::env::var("NEOMAKE_SHELL").ok()?;
    let mut tokens = raw.split_whitespace();
    let program = tokens.next()?.to_string();
    let args: Vec<String> = tokens.map(str::to_string).collect();
    Some(ShellInvocation { program, args })
}

#[cfg(not(windows))]
fn default_for_platform() -> ShellInvocation {
    ShellInvocation::new("sh", &["-c"])
}

#[cfg(windows)]
fn default_for_platform() -> ShellInvocation {
    if which::which("pwsh").is_ok() {
        return ShellInvocation::new("pwsh", &["-NoProfile", "-NoLogo", "-Command"]);
    }
    if which::which("powershell").is_ok() {
        return ShellInvocation::new("powershell", &["-NoProfile", "-NoLogo", "-Command"]);
    }
    tracing::warn!(
        "neither pwsh nor powershell found on PATH; falling back to cmd.exe \
         (POSIX-style commands will not work)"
    );
    ShellInvocation::new("cmd", &["/S", "/C"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_parses_argv() {
        // Use compute() rather than resolve() so we don't poison the
        // global cache for other tests in this binary.
        let saved = std::env::var("NEOMAKE_SHELL").ok();
        // SAFETY: tests in a single-threaded section. We restore below.
        std::env::set_var("NEOMAKE_SHELL", "bash -c");
        let inv = override_from_env().expect("override parsed");
        assert_eq!(inv.program, "bash");
        assert_eq!(inv.args, vec!["-c".to_string()]);
        match saved {
            Some(v) => std::env::set_var("NEOMAKE_SHELL", v),
            None => std::env::remove_var("NEOMAKE_SHELL"),
        }
    }

    #[test]
    fn empty_override_is_ignored() {
        let saved = std::env::var("NEOMAKE_SHELL").ok();
        std::env::set_var("NEOMAKE_SHELL", "   ");
        assert!(override_from_env().is_none());
        match saved {
            Some(v) => std::env::set_var("NEOMAKE_SHELL", v),
            None => std::env::remove_var("NEOMAKE_SHELL"),
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn unix_default_is_sh_dash_c() {
        let inv = default_for_platform();
        assert_eq!(inv.program, "sh");
        assert_eq!(inv.args, vec!["-c".to_string()]);
    }

    #[test]
    #[cfg(windows)]
    fn windows_default_is_a_known_shell() {
        let inv = default_for_platform();
        assert!(matches!(
            inv.program.as_str(),
            "pwsh" | "powershell" | "cmd"
        ));
    }
}

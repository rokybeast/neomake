//! Platform-aware shell resolution for the TOMLX `exec()` built-in.
//!
//! This is a deliberate near-duplicate of [`neomake_core::shell`].
//! `neomake-tomlx` is meant to remain independent of `neomake-core` (the
//! current layering is `core` engine + `tomlx` parsing → consumed by the
//! `neomake` CLI), so we keep this small helper local rather than
//! introducing a `tomlx → core` dependency just for it.
//!
//! Resolution order matches `neomake-core::shell`:
//! 1. `NEOMAKE_SHELL` env var (whitespace-tokenized argv); the command
//!    string is appended as the final argv element.
//! 2. Unix default: `sh -c`.
//! 3. Windows default: `pwsh` > `powershell` > `cmd /S /C`.

use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub(crate) struct ShellInvocation {
    pub program: String,
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

pub(crate) fn resolve() -> ShellInvocation {
    RESOLVED.get_or_init(compute).clone()
}

fn compute() -> ShellInvocation {
    if let Some(inv) = override_from_env() {
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

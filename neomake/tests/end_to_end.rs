//! End-to-end integration tests for the `neomake` CLI.
//!
//! These exercise the whole pipeline (TOMLX load → TaskSet → DAG →
//! executor → cache) by invoking the built binary against a config in a
//! temporary directory.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn neomake_bin() -> PathBuf {
    // `CARGO_BIN_EXE_<name>` is set by cargo when running integration tests.
    PathBuf::from(env!("CARGO_BIN_EXE_neomake"))
}

/// Build a `neomake.toml` whose three tasks (a, b, c) work under
/// either `sh` (Unix default) or PowerShell (Windows default).
///
/// On Windows we use `Set-Content -Encoding ascii` and `Get-Content`
/// because PowerShell 5.1's `>` redirection writes UTF-16 + BOM, which
/// would change file contents and defeat the cache verification.
fn write_basic_config(dir: &std::path::Path) {
    #[cfg(windows)]
    let cfg = r#"
[tasks.a]
command = "Set-Content -Encoding ascii -Path a.txt -Value 'hello-a'"
outputs = ["a.txt"]

[tasks.b]
command = "Set-Content -Encoding ascii -Path b.txt -Value 'hello-b'"
outputs = ["b.txt"]

[tasks.c]
command = "Get-Content a.txt, b.txt | Set-Content -Encoding ascii -Path c.txt"
deps    = ["a", "b"]
inputs  = ["a.txt", "b.txt"]
outputs = ["c.txt"]
"#;
    #[cfg(not(windows))]
    let cfg = r#"
[tasks.a]
command = "echo hello-a > a.txt"
outputs = ["a.txt"]

[tasks.b]
command = "echo hello-b > b.txt"
outputs = ["b.txt"]

[tasks.c]
command = "cat a.txt b.txt > c.txt"
deps    = ["a", "b"]
inputs  = ["a.txt", "b.txt"]
outputs = ["c.txt"]
"#;
    fs::write(dir.join("neomake.toml"), cfg).unwrap();
}

#[test]
fn runs_full_pipeline_then_hits_cache() {
    let tmp = tempfile::tempdir().unwrap();
    write_basic_config(tmp.path());

    let out1 = Command::new(neomake_bin())
        .args(["run", "c"])
        .current_dir(tmp.path())
        .output()
        .expect("spawn neomake");
    assert!(
        out1.status.success(),
        "first run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out1.stdout),
        String::from_utf8_lossy(&out1.stderr)
    );
    let s1 = String::from_utf8_lossy(&out1.stdout);
    assert!(s1.contains("[OK"), "expected an [OK] tag; got: {s1}");

    // Second run must hit the cache for all tasks.
    let out2 = Command::new(neomake_bin())
        .args(["run", "c"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(out2.status.success());
    let s2 = String::from_utf8_lossy(&out2.stdout);
    assert!(
        s2.contains("[CACHED"),
        "expected cache hits on second run; got: {s2}"
    );

    // `cache status` reports nonzero entries.
    let stat = Command::new(neomake_bin())
        .args(["cache", "status"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(stat.status.success());
    let stats = String::from_utf8_lossy(&stat.stdout);
    assert!(stats.contains("entries"));
}

#[test]
fn list_command_prints_topo_order() {
    let tmp = tempfile::tempdir().unwrap();
    write_basic_config(tmp.path());

    let out = Command::new(neomake_bin())
        .arg("list")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    let pos_a = s.find("  a").unwrap();
    let pos_b = s.find("  b").unwrap();
    let pos_c = s.find("  c").unwrap();
    assert!(pos_a < pos_c);
    assert!(pos_b < pos_c);
}

#[test]
fn tomlx_variables_work_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let cfg = r#"
$greeting = "hi"

[tasks.t]
command = "Set-Content -Encoding ascii -Path out.txt -Value '${greeting}'"
outputs = ["out.txt"]
"#;
    #[cfg(not(windows))]
    let cfg = r#"
$greeting = "hi"

[tasks.t]
command = "echo ${greeting} > out.txt"
outputs = ["out.txt"]
"#;
    fs::write(tmp.path().join("neomake.tomlx"), cfg).unwrap();

    let out = Command::new(neomake_bin())
        .args(["run", "t"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "tomlx run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let produced = fs::read_to_string(tmp.path().join("out.txt")).unwrap();
    assert!(produced.contains("hi"), "got: {produced:?}");
}

#[test]
fn cycle_is_reported_with_clear_message() {
    let tmp = tempfile::tempdir().unwrap();
    // Cycle detection runs before any command is invoked, so the
    // command string only has to parse as a TOML value. `echo` exists
    // (or is aliased to `Write-Output`) on every supported platform.
    let cfg = r#"
[tasks.a]
command = "echo a"
deps = ["b"]

[tasks.b]
command = "echo b"
deps = ["a"]
"#;
    fs::write(tmp.path().join("neomake.toml"), cfg).unwrap();

    let out = Command::new(neomake_bin())
        .arg("list")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected cycle error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cycle") || stderr.contains("Cycle"),
        "expected 'cycle' in stderr; got: {stderr}"
    );
}

#[test]
fn failing_task_skips_dependents() {
    let tmp = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let cfg = r#"
[tasks.bad]
command = "exit 1"

[tasks.dep]
command = "echo should-not-run"
deps = ["bad"]
"#;
    #[cfg(not(windows))]
    let cfg = r#"
[tasks.bad]
command = "false"

[tasks.dep]
command = "echo should-not-run"
deps = ["bad"]
"#;
    fs::write(tmp.path().join("neomake.toml"), cfg).unwrap();

    let out = Command::new(neomake_bin())
        .arg("run")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected non-zero exit");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("[FAILED"), "expected a FAILED tag; got: {s}");
    assert!(s.contains("[SKIPPED"), "expected a SKIPPED tag; got: {s}");
}

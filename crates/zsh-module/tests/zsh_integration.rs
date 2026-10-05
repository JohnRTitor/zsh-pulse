//! Stage B acceptance tests: the real thing, run against a real zsh.
//!
//! These are the tests that matter. Every one runs `zsh` as a **subprocess**
//! and asserts on its exit status, so an ABI mistake shows up as a failing test
//! rather than a dead interactive shell.
//!
//! Run with: `cargo test -p zsh-module --test zsh_integration`

use std::path::PathBuf;
use std::process::Command;

/// Locate the built `cdylib`, or fail with an actionable message.
///
/// `CARGO_BUILT_LIB_DIR` is nightly-only, so the artifact is found relative to
/// the test executable (`target/<profile>/deps/<test>`).
fn module_so() -> PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let profile_dir = exe
        .parent()
        .and_then(std::path::Path::parent)
        .expect("target/<profile>");
    let so = profile_dir.join("libshellpulse_highlight.so");
    assert!(
        so.exists(),
        "cdylib not found at {} — run `cargo build -p zsh-module` first",
        so.display()
    );
    so
}

/// Create a temporary `$module_path` containing `shellpulse/highlight.so`.
///
/// zsh requires slash-separated identifiers with no dots, so the module must
/// live at `<dir>/shellpulse/highlight.so` and be loaded as
/// `shellpulse/highlight` (`zsh-src/Src/module.c:2177-2186`).
///
/// Installed **once** per test process: cargo runs these tests in parallel, and
/// concurrent `fs::copy` into one destination produced a truncated `.so` that
/// segfaulted zsh — a test-harness bug that looked exactly like an ABI bug.
static INSTALLED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn install_module() -> &'static PathBuf {
    INSTALLED.get_or_init(|| {
        let base = std::env::temp_dir().join(format!("shell-pulse-it-{}", std::process::id()));
        let dir = base.join("shellpulse");
        std::fs::create_dir_all(&dir).expect("create module dir");
        std::fs::copy(module_so(), dir.join("highlight.so")).expect("copy cdylib");
        base
    })
}

/// Run `zsh -f` with the module on `$module_path`.
///
/// `body` is zsh code run *after* `zmodload shellpulse/highlight`.
/// Returns (success, combined stdout+stderr).
fn run_zsh(body: &str, env: &[(&str, &str)]) -> (bool, String) {
    let mp = install_module();
    let script = format!(
        "module_path=({mp} $module_path)\n\
         zmodload shellpulse/highlight\n\
         {body}\n",
        mp = mp.display()
    );
    let mut cmd = Command::new("zsh");
    cmd.arg("-f").arg("-c").arg(&script);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn zsh");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

#[test]
fn module_loads_into_a_bare_zsh() {
    // `zsh -f`: no user config at all — the hardest environment for us.
    let (ok, out) = run_zsh("print -r -- LOADED", &[]);
    assert!(ok, "zsh -f failed to load the module: {out}");
    assert!(out.contains("LOADED"), "load did not complete: {out}");
}

#[test]
fn builtin_is_registered_under_its_exact_name() {
    // The name must be exactly `shell-pulse-highlight`: a Rust `&str` is not
    // NUL-terminated, and getting that wrong once produced a builtin named
    // `shell-pulse-SHELL_PULSE_DEBUG` and corrupted zsh's builtin table.
    let (ok, out) = run_zsh("whence -w shell-pulse-highlight", &[]);
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("shell-pulse-highlight: builtin"),
        "builtin not registered; got: {out}"
    );
}

#[test]
fn no_garbage_names_appear_in_the_builtin_table() {
    // Regression guard for the NUL-termination bug.
    let (ok, out) = run_zsh("print -rl -- ${(k)builtins} | grep -i shell-pulse", &[]);
    assert!(ok, "zsh exited badly: {out}");
    let lines: Vec<&str> = out.lines().filter(|l| l.contains("shell-pulse")).collect();
    assert_eq!(
        lines,
        vec!["shell-pulse-highlight"],
        "unexpected entries in the builtin table: {lines:?}"
    );
}

#[test]
fn malformed_input_does_not_crash() {
    // Nothing here is valid shell. The engine must not fault, and must not
    // silently produce a highlight it believes in.
    let (ok, out) = run_zsh(
        "for b in '' '\"unterminated' '$((' '`' '|||' 'a=b' '\\' '${'; do\n\
             BUFFER=$b; CURSOR=0; shell-pulse-highlight\n\
         done\n\
         print -r -- SURVIVED",
        &[("SHELL_PULSE_DEBUG", "1")],
    );
    assert!(ok, "zsh exited badly on malformed input: {out}");
    assert!(out.contains("SURVIVED"), "did not reach the end: {out}");
}

#[test]
fn multiline_and_unicode_input_are_handled() {
    let (ok, out) = run_zsh(
        "BUFFER=$'line one\\nline two'\n\
         CURSOR=9\n\
         shell-pulse-highlight\n\
         BUFFER='echo \"héllo wörld ✓ 日本語\"'\n\
         CURSOR=3\n\
         shell-pulse-highlight\n\
         print -r -- SURVIVED",
        &[("SHELL_PULSE_DEBUG", "1")],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(out.contains("SURVIVED"), "did not reach the end: {out}");
}

#[test]
fn repeated_invocation_is_stable() {
    // 5000 calls in one shell: catches leaks, stale pointers and refcount bugs.
    let (ok, out) = run_zsh(
        "BUFFER='cargo build --release'\n\
         CURSOR=5\n\
         for i in {1..5000}; do shell-pulse-highlight; done\n\
         print -r -- REPEATED_OK",
        &[],
    );
    assert!(ok, "zsh exited badly after 5000 calls: {out}");
    assert!(out.contains("REPEATED_OK"), "loop did not finish: {out}");
}

#[test]
fn loading_twice_is_harmless() {
    let mp = install_module();
    let script = format!(
        "module_path=({mp} $module_path)\n\
         zmodload shellpulse/highlight\n\
         zmodload shellpulse/highlight\n\
         whence -w shell-pulse-highlight\n",
        mp = mp.display()
    );
    let out = Command::new("zsh").arg("-f").arg("-c").arg(&script).output().expect("zsh");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "second load broke the shell: {text}");
    assert!(text.contains("builtin"), "builtin lost after reload: {text}");
}

#[test]
fn a_missing_module_fails_cleanly_and_the_shell_survives() {
    // The fallback path the adapter depends on: no module, no damage.
    let empty = std::env::temp_dir().join("shell-pulse-empty-module-path");
    std::fs::create_dir_all(&empty).expect("create empty module path");
    let out = Command::new("zsh")
        .arg("-f")
        .arg("-c")
        .arg("zmodload shellpulse/definitelymissing; print -r -- SHELL_STILL_WORKS")
        .env("module_path", &empty)
        .output()
        .expect("zsh");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("SHELL_STILL_WORKS"), "shell did not survive: {text}");
    assert!(out.status.success(), "shell exited badly: {text}");
}

#[test]
fn zsh_rejects_the_wrong_module_path_forms() {
    // Guards the packaging assumption in `modname_ok()`
    // (`zsh-src/Src/module.c:2177-2186`): an absolute path, a `.so` suffix and a
    // dot-prefixed name are all invalid, so the module must be installed as
    // `shellpulse/highlight.so` and loaded as `shellpulse/highlight`.
    //
    // zsh aborts the script on a bad name (exit status 1), which is expected and
    // harmless. What must never happen is a *signal* — that would mean we
    // corrupted the shell rather than merely being rejected.
    let mp = install_module();
    let abs = mp.join("shellpulse").join("highlight.so");
    for bad in [abs.display().to_string(), "shellpulse/highlight.so".to_string()] {
        let out = Command::new("zsh")
            .arg("-f")
            .arg("-c")
            .arg(format!(
                "module_path=({mp} $module_path)\n\
                 zmodload {bad}\n\
                 whence -w shell-pulse-highlight\n",
                mp = mp.display(),
            ))
            .output()
            .expect("zsh");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();

        assert!(
            !text.contains("shell-pulse-highlight: builtin"),
            "`zmodload {bad}` unexpectedly loaded the module"
        );
        #[cfg(unix)]
        if let Some(sig) = std::os::unix::process::ExitStatusExt::signal(&out.status) {
            panic!("`zmodload {bad}` killed zsh with signal {sig}: {text}");
        }
    }
}
#[test]
fn reads_live_buffer_and_cursor_from_inside_zsh() {
    // The Stage B headline result: live ZLE state crosses the boundary with no
    // copy-out parameter and no IPC.
    let (ok, out) = run_zsh(
        "BUFFER='git commit --amend --no-edit'\n\
         CURSOR=24\n\
         shell-pulse-highlight",
        &[("SHELL_PULSE_DEBUG", "1")],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("git commit --amend --no-edit"),
        "module did not see the live BUFFER; stderr: {out}"
    );
    assert!(out.contains("CURSOR=\"24\""), "CURSOR not read correctly: {out}");
}
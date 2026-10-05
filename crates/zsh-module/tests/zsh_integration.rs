//! Stage B acceptance tests: the real thing, run against a real zsh.
//!
//! These are the tests that matter. Every one runs `zsh` as a **subprocess**
//! and asserts on its exit status, so an ABI mistake shows up as a failing test
//! rather than a dead interactive shell.
//!
//! Run with: `cargo test -p zsh-module --test zsh_integration`

use std::path::{Path, PathBuf};
use std::process::Command;

/// Newest mtime of any `.rs` under `dir`, used to detect a stale cdylib.
fn newest_source_mtime(dir: &std::path::Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let mtime = if path.is_dir() {
            newest_source_mtime(&path)
        } else if path.extension().is_some_and(|e| e == "rs") {
            std::fs::metadata(&path).ok()?.modified().ok()
        } else {
            None
        };
        if let Some(t) = mtime {
            newest = Some(match newest {
                Some(n) if n > t => n,
                _ => t,
            });
        }
    }
    newest
}

/// Locate the built `cdylib`, rebuilding it first if it is stale.
///
/// `CARGO_BUILT_LIB_DIR` is nightly-only, so the artifact is found relative to
/// the test executable (`target/<profile>/deps/<test>`).
///
/// **Why the explicit rebuild:** `cargo test` builds this crate's rlib for the
/// test harness but does not refresh the `cdylib`. Without this check the
/// integration tests silently ran a `libshellpulse_highlight.so` built hours
/// earlier — passing against code that was no longer there.
fn module_so() -> PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let profile_dir = exe
        .parent()
        .and_then(std::path::Path::parent)
        .expect("target/<profile>");
    let so = profile_dir.join("libshellpulse_highlight.so");

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let newest = newest_source_mtime(manifest).expect("scan crate sources");
    let stale = match std::fs::metadata(&so).and_then(|m| m.modified()) {
        Ok(built) => built < newest,
        Err(_) => true,
    };
    if stale {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "zsh-module", "--lib"])
            .current_dir(manifest.parent().expect("workspace root"))
            .status()
            .expect("run cargo build");
        assert!(status.success(), "rebuilding the cdylib failed: {status}");
    }

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
fn worker_thread_runs_inside_real_zsh_and_answers() {
    // Stage B7. Spawns the worker inside a live zsh and round-trips a job
    // through it. If threads were unsafe here this is where it shows.
    let (ok, out) = run_zsh(
        "print -r -- \"threads=$(ls /proc/$$/task | wc -l)\"\n\
         BUFFER='git commit'\n\
         CURSOR=3\n\
         shell-pulse-highlight\n\
         print -r -- WORKER_PATH_OK",
        &[("SHELL_PULSE_DEBUG", "1")],
    );
    assert!(ok, "zsh exited badly with a worker thread running: {out}");
    assert!(
        out.contains("worker ready"),
        "the worker never reported ready; {out}"
    );
    assert!(out.contains("WORKER_PATH_OK"), "shell did not finish: {out}");

    // zsh is single-threaded; if we added ours there must be more than one.
    let threads = out
        .lines()
        .find_map(|l| l.strip_prefix("threads="))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .expect("thread count");
    assert!(
        threads >= 2,
        "expected zsh + at least one worker thread, saw {threads}"
    );
}

#[test]
fn zsh_survives_signals_delivered_while_a_worker_is_alive() {
    // The residual risk named in engine::worker: zsh installs process-wide
    // signal handlers, and a signal can land on our thread, where zsh's handler
    // would run on a stack zsh knows nothing about.
    //
    // SIGWINCH and SIGCONT are harmless to zsh, so any crash here is ours.
    let (ok, out) = run_zsh(
        "for i in {1..300}; do\n\
             kill -WINCH $$\n\
             kill -CONT  $$\n\
         done\n\
         print -r -- SIGNALS_SURVIVED\n\
         shell-pulse-highlight\n\
         print -r -- STILL_WORKS",
        &[("SHELL_PULSE_DEBUG", "1")],
    );
    // `ok` is false for a signal death as well as a non-zero exit, so this
    // covers the crash case too.
    assert!(ok, "zsh died while signals hit a live worker thread: {out}");
    assert!(out.contains("SIGNALS_SURVIVED"), "loop aborted: {out}");
    assert!(out.contains("STILL_WORKS"), "builtin broke after signals: {out}");
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
        out.contains("read BUFFER=\"git commit --amend --no-edit\""),
        "module did not see the live BUFFER; stderr: {out}"
    );
    // CURSOR is parsed to an i64, so it reports as `Some(n)` rather than a
    // quoted string.
    assert!(out.contains("CURSOR=Some(24)"), "CURSOR not read correctly: {out}");
}
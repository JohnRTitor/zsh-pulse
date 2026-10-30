//! Helpers shared by the `zsh-module` integration tests.
//!
//! Two test binaries need the same three things: the path of the built
//! `cdylib`, a `$module_path` zsh will load it from, and a way to run zsh.
//! They live here so that both binaries use *exactly* the same code — a
//! test that loaded a different module than its neighbour would be testing
//! a different thing.
//!
//! `#![allow(dead_code)]`
//!
//! Each test binary compiles this module separately and uses a different
//! subset of it, so the helpers that one binary does not call would
//! otherwise be reported as dead code.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The path of the zsh adapter.
pub fn adapter_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates directory")
        .parent()
        .expect("workspace root")
        .join("zsh")
        .join("init.zsh")
}

/// Newest mtime of any source file in the workspace.
///
/// The whole workspace is scanned, not just this crate: the module links the
/// `lexer`, `highlight` and `zsh-abi` crates, so a change in any of them
/// makes the built `cdylib` stale. Scanning only this crate would let the
/// tests run against a module built from an older lexer — and the pty test,
/// the only end-to-end evidence there is, would happily pass.
fn newest_source_mtime(dir: &std::path::Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    let Ok(entries) = std::fs::read_dir(dir) else { return None };
    for entry in entries.flatten() {
        let path = entry.path();
        let mtime = if path.is_dir() {
            // `target/` holds build artifacts, not sources; the test
            // directories hold tests, not the library.
            //
            // A test file is not part of the library, and treating it as a
            // reason to rebuild would make every edit to a test trigger a
            // `cargo build` from *inside* a test — and that build blocks on
            // the build lock the `cargo test` running it already holds.
            if path.file_name().is_some_and(|n| {
                n == "target" || n == "tests" || n == "benches" || n == "examples"
            }) {
                None
            } else {
                newest_source_mtime(&path)
            }
        } else if path.extension().is_some_and(|e| e == "rs" || e == "toml") {
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
/// **Why the explicit rebuild:** `cargo test` builds this crate's rlib for
/// the test harness but does not refresh the `cdylib`. Without this check the
/// integration tests silently ran a `libzshpulse_core.so` built hours
/// earlier — passing against code that was no longer there.
pub fn module_so() -> PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    let profile_dir = exe
        .parent()
        .and_then(std::path::Path::parent)
        .expect("target/<profile>");
    let so = profile_dir.join("libzshpulse_core.so");

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().expect("workspace root");
    let newest = newest_source_mtime(workspace).expect("scan workspace sources");

    let feature_stamp_path = profile_dir.join("libzshpulse_core.features");
    let current_features = {
        let mut f = Vec::new();
        if cfg!(feature = "highlight") {
            f.push("highlight");
        }
        if cfg!(feature = "autocomplete") {
            f.push("autocomplete");
        }
        if cfg!(any(feature = "history", feature = "history-navigation")) {
            f.push("history");
        }
        f.join(",")
    };
    let feature_changed = match std::fs::read_to_string(&feature_stamp_path) {
        Ok(prev) => prev != current_features,
        Err(_) => true,
    };

    let stale = feature_changed || match std::fs::metadata(&so).and_then(|m| m.modified()) {
        Ok(built) => built < newest,
        Err(_) => true,
    };
    if stale {
        // The target directory has to be passed explicitly: the default one
        // is not where the test executable lives when `CARGO_TARGET_DIR` or
        // `--target-dir` is in use, and the rebuild would then write the
        // module somewhere the tests do not look.
        let target_dir = profile_dir.parent().expect("target directory");
        let mut cmd = Command::new(env!("CARGO"));
        cmd.args(["build", "-p", "zsh-module", "--lib"]);
        #[cfg(not(all(feature = "highlight", feature = "autocomplete", feature = "history")))]
        {
            cmd.arg("--no-default-features");
            if !current_features.is_empty() {
                cmd.arg("--features").arg(&current_features);
            }
        }
        let status = cmd
            .arg("--target-dir")
            .arg(target_dir)
            .current_dir(workspace)
            .status()
            .expect("run cargo build");
        assert!(status.success(), "rebuilding the cdylib failed: {status}");
        let _ = std::fs::write(&feature_stamp_path, &current_features);
    }

    assert!(
        so.exists(),
        "cdylib not found at {} — run `cargo build -p zsh-module` first",
        so.display()
    );
    so
}

/// Create a temporary `$module_path` containing `zshpulse/core.so`.
///
/// zsh requires slash-separated identifiers with no dots, so the module must
/// live at `<dir>/zshpulse/core.so` and be loaded as
/// `zshpulse/core` (`zsh-src/Src/module.c:2177-2186`).
///
/// Installed **once** per test process: cargo runs these tests in parallel, and
/// concurrent `fs::copy` into one destination produced a truncated `.so` that
/// segfaulted zsh — a test-harness bug that looked exactly like an ABI bug.
static INSTALLED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn install_module() -> &'static PathBuf {
    INSTALLED.get_or_init(|| {
        let base = std::env::temp_dir().join(format!("zsh-pulse-it-{}", std::process::id()));
        let dir = base.join("zshpulse");
        std::fs::create_dir_all(&dir).expect("create module dir");
        std::fs::copy(module_so(), dir.join("core.so")).expect("copy cdylib");
        base
    })
}

/// Run `zsh -f` with the module on `$module_path`.
///
/// `body` is zsh code run *after* `zmodload zshpulse/core`.
/// Returns (success, combined stdout+stderr).
pub fn run_zsh(body: &str, env: &[(&str, &str)]) -> (bool, String) {
    let mp = install_module();
    let script = format!(
        "module_path=({mp} $module_path)\n\
         zmodload zshpulse/core\n\
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

/// Whether a usable `zsh` is on `PATH`.
///
/// **Why a missing zsh is a skip and not a failure:** zsh-pulse is a zsh
/// plugin. The module is a zsh loadable module, the adapter is zsh code and
/// the highlighting only exists inside a running zsh. Without a `zsh`
/// binary there is nothing left to assert, so failing would report a
/// defect in the project where the real problem is a missing test
/// dependency.
pub fn zsh_available() -> bool {
    Command::new("zsh")
        .arg("-c")
        .arg("exit 0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Skip the calling test when there is no `zsh` to test against.
///
/// See [`zsh_available`] for why skipping is the right answer rather than
/// failing.
macro_rules! require_zsh {
    () => {
        if !crate::common::zsh_available() {
            eprintln!(
                "SKIPPED: `zsh` is not on PATH. zsh-pulse is a zsh plugin: \
                 without zsh there is no shell to load the module into, so the \
                 assertion could not be made at all."
            );
            return;
        }
    };
}
pub(crate) use require_zsh;

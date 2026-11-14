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

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

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

/// Platform-specific cdylib filename produced by Cargo for this crate.
///
/// The built artifact is named e.g. `libzshpulse_core.so` (Linux),
/// `libzshpulse_core.dylib` (macOS), etc. The module is later installed as
/// `zshpulse/core.so` for `zmodload zshpulse/core` (zsh module naming/layout).
fn cdylib_filename() -> String {
    format!(
        "{}zshpulse_core{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    )
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root from CARGO_MANIFEST_DIR")
        .to_path_buf()
}

fn cargo_target_dir(workspace: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"))
}

fn candidate_profiles() -> Vec<String> {
    let mut profiles = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        for anc in exe.ancestors() {
            if let Some(name) = anc.file_name().and_then(|n| n.to_str()) {
                let under_target = anc
                    .ancestors()
                    .any(|a| a.file_name().is_some_and(|n| n == "target"));
                if under_target && matches!(name, "debug" | "release") {
                    profiles.push(name.to_string());
                }
            }
        }
    }
    profiles.push("debug".to_string());
    profiles.push("release".to_string());
    profiles.sort();
    profiles.dedup();
    profiles
}

fn candidate_cdylib_paths(target_dir: &Path) -> Vec<PathBuf> {
    let lib = cdylib_filename();
    let mut paths = Vec::new();
    for profile in candidate_profiles() {
        paths.push(target_dir.join(&profile).join(&lib));
        if let Ok(triple) = std::env::var("CARGO_BUILD_TARGET") {
            paths.push(target_dir.join(&triple).join(&profile).join(&lib));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        // Fallback hints for unusual test-binary locations (e.g. build-script out dirs).
        for anc in exe.ancestors() {
            if anc
                .file_name()
                .is_some_and(|n| n == "debug" || n == "release")
            {
                paths.push(anc.join(&lib));
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn current_test_features() -> Vec<&'static str> {
    let mut features = Vec::new();
    if cfg!(feature = "highlight") {
        features.push("highlight");
    }
    if cfg!(feature = "autocomplete") {
        features.push("autocomplete");
    }
    // `history-navigation` is an alias used by this crate's feature wiring.
    if cfg!(any(feature = "history", feature = "history-navigation")) {
        features.push("history");
    }
    features
}

fn rebuild_cdylib(workspace: &Path, target_dir: &Path) {
    let mut cmd = Command::new(env!("CARGO"));
    cmd.current_dir(workspace)
        .args(["build", "-p", "zsh-module", "--lib"])
        .arg("--target-dir")
        .arg(target_dir);

    if let Ok(profile) = std::env::var("ZSH_PULSE_CARGO_PROFILE") {
        cmd.args(["--profile", &profile]);
    }

    // Prefer explicit overrides; otherwise mirror the test binary's enabled
    // features so the rebuilt cdylib matches what the integration tests expect.
    if let Ok(features) = std::env::var("ZSH_PULSE_CARGO_FEATURES") {
        if std::env::var("ZSH_PULSE_CARGO_NO_DEFAULT_FEATURES")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        {
            cmd.arg("--no-default-features");
        }
        if !features.is_empty() {
            cmd.args(["--features", &features]);
        }
    } else {
        let features = current_test_features();
        // If all default features are enabled, build with defaults to avoid
        // feature-string drift; otherwise rebuild with the exact subset.
        if !(cfg!(feature = "highlight")
            && cfg!(feature = "autocomplete")
            && cfg!(any(feature = "history", feature = "history-navigation")))
        {
            cmd.arg("--no-default-features");
            if !features.is_empty() {
                cmd.args(["--features", &features.join(",")]);
            }
        }
    }

    if let Ok(target) = std::env::var("CARGO_BUILD_TARGET") {
        cmd.args(["--target", &target]);
    }
    let status = cmd.status().expect("run cargo build for zsh-module cdylib");
    assert!(status.success(), "rebuilding the cdylib failed: {status}");
}

/// Locate the built `cdylib` for the zsh module.
///
/// Resolution is based on Cargo target/profile conventions and environment
/// (`CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET`, test executable location hints)
/// rather than assuming a fixed `target/<profile>/deps/...` layout. The source
/// artifact name is platform-specific (`DLL_PREFIX`/`DLL_SUFFIX`); tests install
/// it as `zshpulse/core.so` for `zmodload zshpulse/core`.
///
/// `cargo test` does not reliably guarantee an up-to-date cdylib for external
/// loading, so this helper always attempts `cargo build -p zsh-module --lib`
/// before resolving the artifact (Cargo's cache keeps repeated builds cheap).
/// You can override build flags with `ZSH_PULSE_CARGO_PROFILE`,
/// `ZSH_PULSE_CARGO_FEATURES`, and `ZSH_PULSE_CARGO_NO_DEFAULT_FEATURES`.
pub fn module_so() -> PathBuf {
    if let Some(p) = std::env::var_os("ZSH_PULSE_CDYLIB_PATH") {
        let p = PathBuf::from(p);
        assert!(
            p.exists(),
            "ZSH_PULSE_CDYLIB_PATH is set but does not exist: {}",
            p.display()
        );
        return p;
    }

    let workspace = workspace_root();
    let target_dir = cargo_target_dir(&workspace);

    // Always attempt a build so tests don't load a missing/stale cdylib. Cargo
    // will no-op when artifacts are already up to date.
    rebuild_cdylib(&workspace, &target_dir);

    let candidates = candidate_cdylib_paths(&target_dir);
    if let Some(found) = candidates.iter().find(|p| p.exists()) {
        return found.clone();
    }

    panic!(
        "cdylib not found after building zsh-module.\n\
         Searched:\n  - {}\n\n\
         Attempted build target dir:\n  - {}\n\n\
         Notes:\n\
         - Source artifact name is platform-specific ({}zshpulse_core{}).\n\
         - Tests copy the artifact to zshpulse/core.so for `zmodload zshpulse/core`.\n\
         - Set ZSH_PULSE_CDYLIB_PATH to override discovery.",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n  - "),
        target_dir.display(),
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX,
    );
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

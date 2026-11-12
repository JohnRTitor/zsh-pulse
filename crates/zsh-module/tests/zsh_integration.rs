//! Stage B acceptance tests: the real thing, run against a real zsh.
//!
//! These are the tests that matter. Every one runs `zsh` as a **subprocess**
//! and asserts on its exit status, so an ABI mistake shows up as a failing test
//! rather than a dead interactive shell.
//!
//! Run with: `cargo test -p zsh-module --test zsh_integration`
//!
//! # Skipping
//!
//! Every test in this file needs a `zsh` binary. When there is none the
//! test skips with a printed reason instead of failing: zsh-pulse is a
//! zsh plugin, so without zsh there is nothing to assert, and a failure
//! would blame the project for a missing test dependency. See
//! `common::zsh_available`.

use std::process::Command;

mod common;

use common::{adapter_path, install_module, require_zsh, run_zsh};

#[test]
fn module_loads_into_a_bare_zsh() {
    require_zsh!();
    // `zsh -f`: no user config at all — the hardest environment for us.
    let (ok, out) = run_zsh("print -r -- LOADED", &[]);
    assert!(ok, "zsh -f failed to load the module: {out}");
    assert!(out.contains("LOADED"), "load did not complete: {out}");
}

#[test]
#[cfg(feature = "highlight")]
fn builtin_is_registered_under_its_exact_name() {
    require_zsh!();
    // The name must be exactly `zsh-pulse-highlight`: a Rust `&str` is not
    // NUL-terminated, and getting that wrong once produced a builtin named
    // `zsh-pulse-ZSH_PULSE_DEBUG` and corrupted zsh's builtin table.
    let (ok, out) = run_zsh("whence -w zsh-pulse-highlight", &[]);
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("zsh-pulse-highlight: builtin"),
        "builtin not registered; got: {out}"
    );
}

#[test]
#[allow(clippy::vec_init_then_push)]
fn no_garbage_names_appear_in_the_builtin_table() {
    require_zsh!();
    // Regression guard for the NUL-termination bug.
    let (ok, out) = run_zsh("print -rl -- ${(k)builtins} | grep -i zsh-pulse", &[]);
    assert!(ok, "zsh exited badly: {out}");
    let mut lines: Vec<&str> = out.lines().filter(|l| l.contains("zsh-pulse")).collect();
    lines.sort_unstable();
    let mut expected = Vec::new();
    #[cfg(any(feature = "autocomplete", feature = "history", feature = "history-navigation"))]
    expected.push("zsh-pulse-complete");
    #[cfg(feature = "highlight")]
    expected.push("zsh-pulse-highlight");
    assert_eq!(
        lines,
        expected,
        "unexpected entries in the builtin table: {lines:?}"
    );
}

#[test]
fn malformed_input_does_not_crash() {
    require_zsh!();
    // Nothing here is valid shell. The engine must not fault, and must not
    // silently produce a highlight it believes in.
    let (ok, out) = run_zsh(
        "for b in '' '\"unterminated' '$((' '`' '|||' 'a=b' '\\' '${'; do\n\
             BUFFER=$b; CURSOR=0; zsh-pulse-highlight\n\
         done\n\
         print -r -- SURVIVED",
        &[("ZSH_PULSE_DEBUG", "1")],
    );
    assert!(ok, "zsh exited badly on malformed input: {out}");
    assert!(out.contains("SURVIVED"), "did not reach the end: {out}");
}

#[test]
fn multiline_and_unicode_input_are_handled() {
    require_zsh!();
    let (ok, out) = run_zsh(
        "BUFFER=$'line one\\nline two'\n\
         CURSOR=9\n\
         zsh-pulse-highlight\n\
         BUFFER='echo \"héllo wörld ✓ 日本語\"'\n\
         CURSOR=3\n\
         zsh-pulse-highlight\n\
         print -r -- SURVIVED",
        &[("ZSH_PULSE_DEBUG", "1")],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(out.contains("SURVIVED"), "did not reach the end: {out}");
}

#[test]
fn repeated_invocation_is_stable() {
    require_zsh!();
    // 5000 calls in one shell: catches leaks, stale pointers and refcount bugs.
    let (ok, out) = run_zsh(
        "BUFFER='cargo build --release'\n\
         CURSOR=5\n\
         for i in {1..5000}; do zsh-pulse-highlight; done\n\
         print -r -- REPEATED_OK",
        &[],
    );
    assert!(ok, "zsh exited badly after 5000 calls: {out}");
    assert!(out.contains("REPEATED_OK"), "loop did not finish: {out}");
}

#[test]
fn loading_twice_is_harmless() {
    require_zsh!();
    let mp = install_module();
    let script = format!(
        "module_path=({mp} $module_path)\n\
         zmodload zshpulse/core\n\
         zmodload zshpulse/core\n\
         whence -w zsh-pulse-highlight\n",
        mp = mp.display()
    );
    let out = Command::new("zsh").arg("-f").arg("-c").arg(&script).output().expect("zsh");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "second load broke the shell: {text}");
    assert!(text.contains("builtin"), "builtin lost after reload: {text}");
}

#[test]
fn worker_thread_runs_inside_real_zsh_and_answers() {
    require_zsh!();
    // The thread count is read from `/proc`, which only exists on Linux.
    // Everywhere else the worker still has to start and answer — that part
    // is checked below — but the *count* cannot be taken, so the test skips
    // rather than failing on a platform it was never written for.
    if !std::path::Path::new("/proc/self/task").is_dir() {
        eprintln!(
            "SKIPPED: `/proc` is not mounted, so the thread count of the zsh \
             process cannot be read. The worker itself is exercised by the \
             other tests in this file."
        );
        return;
    }
    // Stage B7. Spawns the worker inside a live zsh and round-trips a job
    // through it. If threads were unsafe here this is where it shows.
    let (ok, out) = run_zsh(
        "print -r -- \"threads=$(ls /proc/$$/task | wc -l)\"\n\
         BUFFER='git commit'\n\
         CURSOR=3\n\
         zsh-pulse-highlight\n\
         print -r -- WORKER_PATH_OK",
        &[("ZSH_PULSE_DEBUG", "1")],
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
    require_zsh!();
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
         zsh-pulse-highlight\n\
         print -r -- STILL_WORKS",
        &[("ZSH_PULSE_DEBUG", "1")],
    );
    // `ok` is false for a signal death as well as a non-zero exit, so this
    // covers the crash case too.
    assert!(ok, "zsh died while signals hit a live worker thread: {out}");
    assert!(out.contains("SIGNALS_SURVIVED"), "loop aborted: {out}");
    assert!(out.contains("STILL_WORKS"), "builtin broke after signals: {out}");
}

#[test]
fn a_missing_module_fails_cleanly_and_the_shell_survives() {
    require_zsh!();
    // The fallback path the adapter depends on: no module, no damage.
    let empty = std::env::temp_dir().join("zsh-pulse-empty-module-path");
    std::fs::create_dir_all(&empty).expect("create empty module path");
    let out = Command::new("zsh")
        .arg("-f")
        .arg("-c")
        .arg("zmodload zshpulse/definitelymissing; print -r -- SHELL_STILL_WORKS")
        .env("module_path", &empty)
        .output()
        .expect("zsh");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("SHELL_STILL_WORKS"), "shell did not survive: {text}");
    assert!(out.status.success(), "shell exited badly: {text}");
}

#[test]
fn zsh_rejects_the_wrong_module_path_forms() {
    require_zsh!();
    // Guards the packaging assumption in `modname_ok()`
    // (`zsh-src/Src/module.c:2177-2186`): an absolute path, a `.so` suffix and a
    // dot-prefixed name are all invalid, so the module must be installed as
    // `zshpulse/core.so` and loaded as `zshpulse/core`.
    //
    // zsh aborts the script on a bad name (exit status 1), which is expected and
    // harmless. What must never happen is a *signal* — that would mean we
    // corrupted the shell rather than merely being rejected.
    let mp = install_module();
    let abs = mp.join("zshpulse").join("core.so");
    for bad in [abs.display().to_string(), "zshpulse/core.so".to_string()] {
        let out = Command::new("zsh")
            .arg("-f")
            .arg("-c")
            .arg(format!(
                "module_path=({mp} $module_path)\n\
                 zmodload {bad}\n\
                 whence -w zsh-pulse-highlight\n",
                mp = mp.display(),
            ))
            .output()
            .expect("zsh");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();

        assert!(
            !text.contains("zsh-pulse-highlight: builtin"),
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
    require_zsh!();
    // The Stage B headline result: live ZLE state crosses the boundary with no
    // copy-out parameter and no IPC.
    let (ok, out) = run_zsh(
        "BUFFER='git commit --amend --no-edit'\n\
         CURSOR=24\n\
         zsh-pulse-highlight",
        &[("ZSH_PULSE_DEBUG", "1")],
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

#[test]
fn a_parameter_written_repeatedly_is_readable_and_does_not_corrupt_the_shell() {
    require_zsh!();
    // Regression guard for the allocator bug that blocked Stage C.
    //
    // `setaparam` takes ownership of the value array and `freearray` releases
    // it with `free()`. The array must therefore come from `zalloc` (plain
    // malloc) and the elements from `ztrdup`. The pool allocator `zhalloc`
    // (and `dupstring`, which uses it) looks equivalent but is not: pool
    // memory is rewound by `popheap()` around every command, so the parameter
    // ends up pointing at recycled memory.
    //
    // The observable failure is a `SIGSEGV` inside `sepjoin` when the
    // parameter is read back, or a glibc "free(): invalid size" abort.
    let (ok, out) = run_zsh(
        "export ZSH_PULSE_ECHO=1\n\
         for i in {1..25}; do\n\
             BUFFER=\"value $i\"\n\
             CURSOR=3\n\
             zsh-pulse-highlight\n\
         done\n\
         # A command between the write and the read: this is what triggers\n\
         # zsh's pushheap/popheap pair and recycles pool memory.\n\
         :\n\
         print -r -- \"STATE=$ZSH_PULSE_STATE\"",
        &[],
    );
    assert!(ok, "zsh died after repeated parameter writes: {out}");
    assert!(
        out.contains("STATE=25:3:value 25"),
        "the parameter did not read back the last value written: {out}"
    );
}

#[test]
fn a_parameter_written_repeatedly_keeps_the_shell_usable() {
    require_zsh!();
    // The same bug, seen from the other side: the shell must remain able to
    // run commands, expand the parameter and exit cleanly after many writes.
    let (ok, out) = run_zsh(
        "export ZSH_PULSE_ECHO=1\n\
         for i in {1..200}; do\n\
             BUFFER=\"value $i\"\n\
             CURSOR=1\n\
             zsh-pulse-highlight\n\
         done\n\
         # `setaparam` always assigns an *array* (zsh-src/Src/params.c:3318),\n\
         # so the value is element 1 of a one-element array.\n\
         print -r -- \"VALUE=${ZSH_PULSE_STATE[1]}\"\n\
         print -r -- STILL_USABLE",
        &[],
    );
    assert!(ok, "zsh died after 200 parameter writes: {out}");
    assert!(
        out.contains("VALUE=200:1:value 200"),
        "the parameter did not read back the last value written: {out}"
    );
    assert!(out.contains("STILL_USABLE"), "shell did not finish: {out}");
}

#[test]
fn the_highlighter_fills_region_highlight() {
    require_zsh!();
    // The whole write path: read the line, highlight it, hand the result to
    // zsh. A global `region_highlight` stands in for the ZLE special
    // parameter, which only exists while a widget is running.
    //
    // The answer is given on the command line: the module only colours a
    // command word the shell was asked about, so the call has to say that
    // `echo` is a command the shell knows. Without the answer there would
    // be no region at all, and the test would prove nothing about the
    // write path.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         BUFFER='echo hi'\n\
         CURSOR=3\n\
         zsh-pulse-highlight --region-highlight --command-known=echo\n\
         print -r -- \"REGIONS=<${(j:,:)region_highlight}>\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    // The delimiters make the assertion about the *whole* array: a
    // duplicated or an extra region would change it.
    // The assertion is on the *whole* array, so a duplicate or a missing
    // region would fail it.
    assert!(
        out.contains("REGIONS=<0 4 fg=green memo=zsh-pulse>"),
        "unexpected regions: {out}"
    );
    // `hi` is an ordinary argument: it is not highlighted at all.
    assert!(
        !out.contains("5 7"),
        "only the command should have been highlighted: {out}"
    );
}

#[test]
fn the_highlighter_reports_character_offsets_not_byte_offsets() {
    require_zsh!();
    // zsh's `region_highlight` uses the same units as `CURSOR`, which count
    // characters. A multi-byte character must not shift the offsets.
    //
    // Both command words of the line are answered for: `echo` and `grep`
    // are two commands, and the second one is just as much a command as
    // the first.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         BUFFER=$'echo \\u00e9 | grep x'\n\
         CURSOR=3\n\
         zsh-pulse-highlight --region-highlight --command-known=echo --command-known=grep\n\
         print -r -- \"REGIONS=<${(j:,:)region_highlight}>\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    // `echo` is characters 0 to 4; the `|` operator is character 7 (it is
    // byte 8, because `é` takes two bytes). A byte-based implementation
    // would report 8.
    assert!(
        out.contains(
            "REGIONS=<0 4 fg=green memo=zsh-pulse,7 8 fg=red memo=zsh-pulse,\
             9 13 fg=green memo=zsh-pulse>"
        ),
        "the offsets must be characters, not bytes: {out}"
    );
}

#[test]
fn the_highlighter_does_not_create_a_global_region_highlight() {
    require_zsh!();
    // The ZLE special parameters only exist while a widget is running.
    // Writing to `region_highlight` outside a widget would create a *global*
    // parameter of that name in the user's shell, which is a bug, not a
    // feature.
    let (ok, out) = run_zsh(
        "BUFFER='echo hi'\n\
         CURSOR=3\n\
         zsh-pulse-highlight --region-highlight\n\
         print -r -- \"EXISTS=${+parameters[region_highlight]}\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("EXISTS=0"),
        "the module must not create a global region_highlight: {out}"
    );
}

#[test]
fn the_regions_are_not_published_unless_the_caller_asks() {
    require_zsh!();
    // The regions are only published when the caller passes the flag. That
    // is what keeps the module from overwriting a global `region_highlight`
    // that the user or another plugin owns.
    //
    // The answer is given, so that there *is* a region the module would
    // publish: without it, the test would pass even if the module
    // published on every call.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=(SENTINEL)\n\
         WIDGET=test-widget\n\
         BUFFER='echo hi'\n\
         CURSOR=3\n\
         zsh-pulse-highlight --command-known=echo\n\
         print -r -- \"REGIONS=${(j:,:)region_highlight}\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("REGIONS=SENTINEL"),
        "the regions must not be published without the flag: {out}"
    );
}

#[test]
fn an_unchanged_line_is_not_pushed_again() {
    require_zsh!();
    // The ZLE special parameters are recreated for every widget call, but
    // the array that drives the display is not. So when nothing about the
    // line changed, the module must leave the display alone instead of
    // writing the same regions again.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         BUFFER='echo hi'\n\
         CURSOR=3\n\
         zsh-pulse-highlight --region-highlight\n\
         region_highlight=(SENTINEL)\n\
         zsh-pulse-highlight --region-highlight\n\
         print -r -- \"REGIONS=${(j:,:)region_highlight}\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("REGIONS=SENTINEL"),
        "an unchanged line must not be pushed again: {out}"
    );
}

#[test]
fn a_changed_line_is_pushed_again() {
    require_zsh!();
    // The counterpart of the test above: a real change must be pushed.
    //
    // The answer is given so that there *is* a region to push: a command
    // word the module was not told about gets no region at all.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         BUFFER='echo hi'\n\
         CURSOR=3\n\
         zsh-pulse-highlight --region-highlight --command-known=echo\n\
         region_highlight=(SENTINEL)\n\
         BUFFER='echo there'\n\
         CURSOR=3\n\
         zsh-pulse-highlight --region-highlight --command-known=echo\n\
         print -r -- \"REGIONS=${(j:,:)region_highlight}\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("REGIONS=SENTINEL,0 4 fg=green memo=zsh-pulse"),
        "a changed line must be pushed again, keeping what another plugin \
         had: {out}"
    );
}

#[test]
fn a_line_that_cannot_be_parsed_is_left_unhighlighted() {
    require_zsh!();
    // Nesting deeper than the lexer can represent exactly. The highlights we
    // published for the previous line must be cleared: they describe a line
    // that is no longer there.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         BUFFER='echo hi'\n\
         CURSOR=3\n\
         zsh-pulse-highlight --region-highlight\n\
         BUFFER=$'$(($(($(($(($(($(x)))))))))'\n\
         CURSOR=1\n\
         zsh-pulse-highlight --region-highlight\n\
         print -r -- \"REGIONS=${(j:,:)region_highlight}\"\n\
         print -r -- SURVIVED",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(out.contains("SURVIVED"), "the shell did not finish: {out}");
    assert!(
        !out.contains("fg=green") && !out.contains("fg=red"),
        "an unparseable line must not keep the old highlights: {out}"
    );
}

#[test]
fn the_adapter_is_valid_zsh_and_does_not_break_the_shell() {
    require_zsh!();
    // The adapter is what a user sources from their `.zshrc`. It must parse,
    // and sourcing it in a shell without a terminal must not fail.
    let adapter = adapter_path();
    assert!(adapter.exists(), "the adapter is missing at {}", adapter.display());

    let out = Command::new("zsh")
        .arg("-n")
        .arg(&adapter)
        .output()
        .expect("run zsh -n");
    assert!(
        out.status.success(),
        "the adapter does not parse: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mp = install_module();
    let out = Command::new("zsh")
        .arg("-f")
        .arg("-c")
        .arg(format!(
            "module_path=({mp} $module_path)\n\
             source {adapter}\n\
             print -r -- ADAPTER_OK\n\
             whence -w zsh-pulse-highlight",
            mp = mp.display(),
            adapter = adapter.display(),
        ))
        .output()
        .expect("run zsh");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "sourcing the adapter failed: {text}");
    assert!(text.contains("ADAPTER_OK"), "the adapter broke the shell: {text}");
    assert!(
        text.contains("zsh-pulse-highlight: builtin"),
        "the module was not loaded: {text}"
    );
}

#[test]
fn the_adapter_chains_an_existing_pre_redraw_hook() {
    // The hook is installed with zsh's `add-zle-hook-widget`, which keeps
    // the widgets that are already registered and calls them as well. Using
    // `zle -N` instead would *replace* them, and the plugin that was
    // replaced would stop working — silently, and only after the first
    // command line, because that is when the other plugin installs its own
    // wrapper.
    let adapter = adapter_path();
    let out = Command::new("zsh")
        .arg("-f")
        .arg("-c")
        .arg(format!(
            "zle -N zle-line-pre-redraw my-own-hook\n\
             source {adapter}\n\
             print -r -- \"WIDGET=${{widgets[zle-line-pre-redraw]}}\"\n\
             print -r -- \"LIST=$(zstyle -a zle-line-pre-redraw widgets 2>/dev/null; print -r -- ${{(j:,:)widgets}})\"",
            adapter = adapter.display(),
        ))
        .output()
        .expect("run zsh");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "sourcing the adapter failed: {text}");
    // Both widgets are registered: the one that was there before, and ours.
    assert!(
        text.contains("my-own-hook"),
        "the hook that was already installed must be kept: {text}"
    );
    assert!(
        text.contains("zsh-pulse-highlight-redraw"),
        "our widget must be registered too: {text}"
    );
}

#[test]
fn the_adapter_installs_both_hooks_and_does_not_chain_itself() {
    // The `zle-line-init` hook is what tells the module that a new line is
    // starting; without it a recalled command would be drawn with no
    // highlighting at all. Sourcing the adapter twice must not make the hook
    // call itself.
    let adapter = adapter_path();
    let out = Command::new("zsh")
        .arg("-f")
        .arg("-c")
        .arg(format!(
            "source {adapter}\n\
             source {adapter}\n\
             print -r -- \"PRE=$(zstyle -a zle-line-pre-redraw widgets 2>/dev/null; print -r -- ${{(j:,:)widgets}})\"\n\
             print -r -- \"INIT=$(zstyle -a zle-line-init widgets 2>/dev/null; print -r -- ${{(j:,:)widgets}})\"",
            adapter = adapter.display(),
        ))
        .output()
        .expect("run zsh");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "sourcing the adapter failed: {text}");
    // The style's default value is every built-in ZLE widget, so the list is
    // long; what matters is that our widget is in it, exactly once.
    let pre = text
        .lines()
        .find_map(|l| l.strip_prefix("PRE="))
        .unwrap_or_default();
    let init = text
        .lines()
        .find_map(|l| l.strip_prefix("INIT="))
        .unwrap_or_default();
    assert_eq!(
        pre.matches("zsh-pulse-highlight-redraw").count(),
        1,
        "the redraw hook must be installed exactly once: {text}"
    );
    assert_eq!(
        init.matches("zsh-pulse-highlight-line-init").count(),
        1,
        "the line-init hook must be installed exactly once: {text}"
    );
}

#[test]
fn every_adapter_file_parses() {
    // A syntax error in `widgets.zsh` makes the `source` inside `init.zsh`
    // fail, and zsh carries on — so the adapter would look half-installed.
    let dir = adapter_path().parent().expect("the zsh directory").to_path_buf();
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("read the zsh directory") {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_some_and(|e| e == "zsh") {
            let out = Command::new("zsh").arg("-n").arg(&path).output().expect("run zsh -n");
            assert!(
                out.status.success(),
                "{} does not parse: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr)
            );
            checked += 1;
        }
    }
    assert!(checked >= 2, "expected the adapter to be more than one file");
}

#[test]
fn another_plugins_entries_survive_our_publish() {
    // `region_highlight` is replaced wholesale, so publishing only our own
    // entries would delete everyone else's. The entries we published last
    // time are recognised by their `memo=` and replaced; everything else is
    // kept.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=(SENTINEL)\n\
         WIDGET=test-widget\n\
         BUFFER='echo hi'\n\
         CURSOR=3\n\
         zsh-pulse-highlight --region-highlight --command-known=echo\n\
         print -r -- \"REGIONS=<${(j:,:)region_highlight}>\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("REGIONS=<SENTINEL,0 4 fg=green memo=zsh-pulse>"),
        "another plugin's entry must survive our publish: {out}"
    );
}

#[test]
fn our_own_previous_entries_are_replaced_not_accumulated() {
    // The memo is what tells us which entries are ours. Without replacing
    // them, every keystroke would add another copy.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         for w in git gitx gitxx; do\n\
             BUFFER=\"$w commit\"\n\
             CURSOR=3\n\
             zsh-pulse-highlight --region-highlight --command-known=$w\n\
         done\n\
         print -r -- \"REGIONS=<${(j:,:)region_highlight}>\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("REGIONS=<0 5 fg=green memo=zsh-pulse>"),
        "only our latest entry must be published: {out}"
    );
}

// ---------------------------------------------------------------------------
// The command word: green when the shell knows it, red when it does not
// ---------------------------------------------------------------------------

/// The path of the adapter's widget file.
///
/// The widget file is what asks the shell what it knows about the command
/// word, so the tests that need that answer have to source it.
fn widgets_path() -> std::path::PathBuf {
    adapter_path()
        .parent()
        .expect("the zsh directory")
        .join("widgets.zsh")
}

/// Quote a string for zsh.
///
/// The strings go into the script as single-quoted words, so that `$`,
/// `` ` `` and `\` mean nothing to the shell.
fn zsh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The regions the module publishes for `buffer`.
///
/// The adapter is sourced and its redraw hook is called, so the shell is
/// asked what it knows about the command word: the whole path, exactly as
/// it runs in the user's shell.
fn regions_for(buffer: &str) -> String {
    let widgets = widgets_path();
    let (ok, out) = run_zsh(
        &format!(
            "source {widgets}\n\
             typeset -ga region_highlight\n\
             region_highlight=()\n\
             WIDGET=test-widget\n\
             BUFFER={buffer}\n\
             CURSOR=1\n\
             zsh-pulse-highlight-redraw\n\
             print -r -- \"REGIONS=<${{(j:,:)region_highlight}}>\"",
            widgets = widgets.display(),
            buffer = zsh_quote(buffer),
        ),
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    out
}

fn regions_for_with_cursor(buffer: &str, cursor: usize) -> String {
    let widgets = widgets_path();
    let (ok, out) = run_zsh(
        &format!(
            "source {widgets}\n\
             typeset -ga region_highlight\n\
             region_highlight=()\n\
             WIDGET=test-widget\n\
             BUFFER={buffer}\n\
             CURSOR={cursor}\n\
             zsh-pulse-highlight-redraw\n\
             print -r -- \"REGIONS=<${{(j:,:)region_highlight}}>\"",
            widgets = widgets.display(),
            buffer = zsh_quote(buffer),
            cursor = cursor,
        ),
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    out
}

/// The regions the module publishes for `buffer`, with the shell in `dir`.
///
/// A relative path is resolved against the shell's own `$PWD`, so a test
/// about a file in a particular directory has to put the shell there.
fn regions_for_in(dir: &std::path::Path, buffer: &str) -> String {
    let widgets = widgets_path();
    let (ok, out) = run_zsh(
        &format!(
            "cd {dir}\n\
             source {widgets}\n\
             typeset -ga region_highlight\n\
             region_highlight=()\n\
             WIDGET=test-widget\n\
             BUFFER={buffer}\n\
             CURSOR=1\n\
             zsh-pulse-highlight-redraw\n\
             print -r -- \"REGIONS=<${{(j:,:)region_highlight}}>\"",
            dir = zsh_quote(&dir.display().to_string()),
            widgets = widgets.display(),
            buffer = zsh_quote(buffer),
        ),
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    out
}

/// The regions the module publishes for `buffer`, with the shell's answer
/// given on the command line.
///
/// This is the module on its own: the caller says what the shell knows
/// about the command word, which is what the adapter does in the user's
/// shell.
fn regions_with_answer(buffer: &str, answer: &str) -> String {
    let (ok, out) = run_zsh(
        &format!(
            "typeset -ga region_highlight\n\
             region_highlight=()\n\
             WIDGET=test-widget\n\
             BUFFER={buffer}\n\
             CURSOR=1\n\
             zsh-pulse-highlight --region-highlight {answer}\n\
             print -r -- \"REGIONS=<${{(j:,:)region_highlight}}>\"",
            buffer = zsh_quote(buffer),
            answer = answer,
        ),
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    out
}

#[test]
fn a_command_the_shell_knows_is_green() {
    require_zsh!();
    let out = regions_for("ls");
    assert!(
        out.contains("REGIONS=<0 2 fg=green memo=zsh-pulse>"),
        "a command the shell knows must be green: {out}"
    );
}

#[test]
fn the_shells_own_tables_decide_what_the_shell_knows() {
    require_zsh!();
    // The shell knows a word because of its own tables: an alias, a global
    // alias, a function, a suffix alias, a builtin, a command on `$PATH` or
    // a reserved word. Each of them is a different table, and each of them
    // has to be consulted — a lookup that only asked about `$commands`
    // would call an alias unknown.
    //
    // The names are made up, so they cannot be a command on `$PATH` by
    // accident.
    let widgets = widgets_path();
    let (ok, out) = run_zsh(
        &format!(
            "source {widgets}\n\
             alias sp-alias='echo'\n\
             galiases[sp-galias]='echo'\n\
             sp-function() {{ :; }}\n\
             saliases[sp-suffix]='echo'\n\
             typeset -ga region_highlight\n\
             region_highlight=()\n\
             WIDGET=test-widget\n\
             for w in sp-alias sp-galias sp-function sp-suffix if; do\n\
                 BUFFER=$w\n\
                 CURSOR=1\n\
                 region_highlight=()\n\
                 zsh-pulse-highlight-redraw\n\
                 print -r -- \"WORD=$w REGIONS=<${{(j:,:)region_highlight}}>\"\n\
             done",
            widgets = widgets.display(),
        ),
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    let cases = [
        ("sp-alias", "fg=green"),
        ("sp-galias", "bg=blue"),
        ("sp-function", "fg=green"),
        ("sp-suffix", "fg=green"),
        ("if", "fg=yellow"),
    ];
    for (word, attr) in cases {
        let expected = format!("WORD={word} REGIONS=<0 {} {attr} memo=zsh-pulse>", word.len());
        assert!(
            out.contains(&expected),
            "the shell knows `{word}` through its own tables, so it must have attribute {attr}: {out}"
        );
    }
}

#[test]
fn the_command_words_of_a_line_are_found_the_way_the_shell_finds_them() {
    require_zsh!();
    // The adapter walks the line the way the shell walks it. Every one of
    // these lines has a different shape, and a word that is not a command
    // must not be answered for: an answer about the wrong word is a
    // *wrong colour*, not a missing one.
    let cases: &[(&str, &[&str])] = &[
        // The command words of the line, in order.
        ("ls", &["ls"]),
        ("ls | grep x", &["ls", "grep"]),
        ("ls; grep x", &["ls", "grep"]),
        ("ls && grep x", &["ls", "grep"]),
        ("ls || grep x", &["ls", "grep"]),
        ("ls |& grep x", &["ls", "grep"]),
        ("FOO=bar ls", &["ls"]),
        ("FOO+=bar ls", &["ls"]),
        // A redirection and its target are not commands.
        ("ls > out.txt grep", &["ls"]),
        ("ls >out.txt grep", &["ls"]),
        // A file-descriptor duplication writes to no file, so the number it
        // duplicates is not a command either.
        ("ls 2>&1 | grep x", &["ls", "grep"]),
        // A redirection in front of the command does not hide it.
        ("> out ls", &["ls"]),
        // `&>` redirects both streams: it has a file, so the next word is
        // its target and not a command.
        ("ls &>all", &["ls"]),
        // The command inside a substitution is a command.
        ("echo $(ls)", &["echo", "ls"]),
    ];
    for (line, expected) in cases {
        let out = regions_for(line);
        for word in *expected {
            let green = "fg=green memo=zsh-pulse";
            assert!(
                out.contains(green),
                "`{word}` of `{line}` is a command the shell knows, so it \
                 must be green: {out}"
            );
        }
        // Every command word is answered: the number of green regions is
        // the number of command words.
        let greens = out.matches("fg=green").count();
        assert_eq!(
            greens,
            expected.len(),
            "`{line}` has the command words {expected:?}, so it must have \
             that many green regions: {out}"
        );
    }
}

#[test]
fn a_command_the_shell_cannot_be_asked_about_is_left_alone() {
    require_zsh!();
    // The shell's tables are what say whether the shell knows a command.
    // If they cannot be reached — `zsh/parameter` could not be loaded, say
    // — the shell cannot answer, and the answer is *nothing*, not "no".
    //
    // An answer of "no" would paint every command word red, which says
    // "the shell does not know this command" about a command the shell was
    // never asked about.
    let widgets = widgets_path();
    let (ok, out) = run_zsh(
        &format!(
            "source {widgets}\n\
             zsh-pulse-highlight-can-ask() {{ return 1 }}\n\
             typeset -ga region_highlight\n\
             region_highlight=()\n\
             WIDGET=test-widget\n\
             BUFFER='definitelynotacommand'\n\
             CURSOR=1\n\
             zsh-pulse-highlight-redraw\n\
             print -r -- \"REGIONS=<${{(j:,:)region_highlight}}>\"",
            widgets = widgets.display(),
        ),
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        !out.contains("fg=red") && !out.contains("fg=green"),
        "a command word the shell could not be asked about must be left \
         alone: {out}"
    );
}

#[test]
fn a_command_the_shell_does_not_know_is_red() {
    require_zsh!();
    let out = regions_for("definitelynotacommand");
    assert!(
        out.contains("REGIONS=<0 21 fg=red,bold memo=zsh-pulse>"),
        "a command the shell does not know must be red: {out}"
    );
}

#[test]
fn a_builtin_is_green() {
    require_zsh!();
    // A builtin is in the shell's own table, so it is known even when
    // there is no such file anywhere.
    let out = regions_for("echo");
    assert!(
        out.contains("REGIONS=<0 4 fg=green memo=zsh-pulse>"),
        "a builtin must be green: {out}"
    );
}

#[test]
fn a_leading_assignment_does_not_hide_the_command() {
    require_zsh!();
    // `FOO=bar cmd` runs `cmd`: the command word is the first word that
    // is not an assignment.
    let out = regions_for("FOO=bar ls");
    assert!(
        out.contains(
            "REGIONS=<0 7 fg=yellow memo=zsh-pulse,8 10 fg=green memo=zsh-pulse>"
        ),
        "the command word is the first word that is not an assignment: {out}"
    );
}

#[test]
fn a_leading_assignment_does_not_hide_an_unknown_command() {
    require_zsh!();
    let out = regions_for("FOO=bar definitelynotacommand");
    assert!(
        out.contains(
            "REGIONS=<0 7 fg=yellow memo=zsh-pulse,\
             8 29 fg=red,bold memo=zsh-pulse>"
        ),
        "the command word is the first word that is not an assignment: {out}"
    );
}

#[test]
fn an_answer_about_another_word_is_not_applied() {
    require_zsh!();
    // The shell and the lexer do not always agree on where the command
    // word is. When they do not, the answer must be dropped rather than
    // applied to a word it is not about: colouring the wrong word red
    // would be worse than not colouring it at all.
    let out = regions_with_answer("ls", "--command-unknown=other");
    assert!(
        !out.contains("fg=red"),
        "the wrong word must never be coloured red: {out}"
    );
    // An answer about another word is no answer at all: the command word
    // is left alone rather than painted green.
    assert!(
        !out.contains("fg=green"),
        "a command word without an answer must be left alone: {out}"
    );
}

#[test]
fn the_shells_answer_is_applied_to_the_command_word() {
    require_zsh!();
    // The other side of the test above: when the answer is about the word
    // the lexer found, it is applied.
    let out = regions_with_answer("ls", "--command-unknown=ls");
    assert!(
        out.contains("REGIONS=<0 2 fg=red,bold memo=zsh-pulse>"),
        "the answer must be applied to the command word: {out}"
    );
}

#[test]
fn the_arguments_are_not_coloured_as_commands() {
    require_zsh!();
    // Only the command word is a command. `ls` here is an argument, and it
    // must not be coloured like one.
    let out = regions_for("echo ls");
    assert!(
        out.contains("REGIONS=<0 4 fg=green memo=zsh-pulse>"),
        "only the command word may be a command: {out}"
    );
}

#[test]
fn a_command_word_without_an_answer_is_left_alone() {
    require_zsh!();
    // The module cannot see the shell's tables, so it can only colour a
    // command word the shell was asked about. When it was not asked — as
    // here, where the module is called on its own — the command word is
    // left exactly as the user typed it.
    //
    // Painting it green would be a lie: green means "the shell knows this
    // command", and nobody said so.
    let out = regions_with_answer("ls", "");
    assert!(
        !out.contains("fg=green") && !out.contains("fg=red"),
        "a command word without an answer must not be coloured: {out}"
    );
}

#[test]
fn every_command_word_of_a_pipeline_is_coloured() {
    require_zsh!();
    // `ls | grep x` runs two commands, and the second one is just as much a
    // command as the first. The adapter answers for both, and the module
    // colours both.
    //
    // The argument `x` is not a command and must not be coloured.
    let out = regions_for("ls | grep x");
    assert!(
        out.contains("0 2 fg=green memo=zsh-pulse"),
        "the first command of the pipeline must be green: {out}"
    );
    assert!(
        out.contains("5 9 fg=green memo=zsh-pulse"),
        "the second command of the pipeline must be green too: {out}"
    );
    assert!(
        !out.contains("10 11"),
        "the argument of the pipeline must not be coloured: {out}"
    );
}

#[test]
fn an_unknown_command_of_a_pipeline_is_red() {
    require_zsh!();
    // The other side of the test above: a command the shell does not know
    // is red, wherever it stands in the line.
    //
    // This is the case that used to be wrong: the second command word had
    // no answer and fell back to the lexer's own style, which is green —
    // so an unknown command in a pipeline looked like one the shell knew.
    let out = regions_for("ls | definitelynotacommand");
    assert!(
        out.contains("0 2 fg=green memo=zsh-pulse"),
        "the first command of the pipeline must be green: {out}"
    );
    assert!(
        out.contains("5 26 fg=red,bold memo=zsh-pulse"),
        "the unknown command of the pipeline must be red: {out}"
    );
}

#[test]
fn a_redirection_does_not_hide_the_command() {
    require_zsh!();
    // `cmd > out` runs `cmd`: the file the output goes to is not a
    // command, and the command word is still the command word.
    let out = regions_for("echo > out.txt");
    assert!(
        out.contains("0 4 fg=green memo=zsh-pulse"),
        "a redirection must not hide the command word: {out}"
    );
}

#[test]
fn a_file_descriptor_duplication_does_not_hide_the_command() {
    require_zsh!();
    // `2>&1` duplicates a file descriptor and writes to no file, so the
    // number it duplicates must not be mistaken for the command word.
    let out = regions_for("echo 2>&1");
    assert!(
        out.contains("REGIONS=<0 4 fg=green memo=zsh-pulse,5 8 fg=red memo=zsh-pulse>"),
        "a file-descriptor duplication must not hide the command word, and \\
         the number it duplicates must not be coloured: {out}"
    );
}

#[test]
fn the_old_answer_protocol_is_not_honoured() {
    require_zsh!();
    // The protocol used to be `--command-word=<word>` plus a separate
    // `--command-known` flag. A stale adapter that still speaks it must
    // not paint every command word red: an answer that is not understood
    // is no answer at all.
    let out = regions_with_answer("ls", "--command-word=ls --command-unknown");
    assert!(
        !out.contains("fg=red"),
        "the old protocol must not be honoured: {out}"
    );
}

#[test]
fn an_option_is_cyan() {
    require_zsh!();
    let out = regions_for("git commit --amend");
    assert!(
        out.contains(
            "REGIONS=<0 3 fg=green memo=zsh-pulse,11 18 fg=cyan memo=zsh-pulse>"
        ),
        "an option must be cyan: {out}"
    );
}

#[test]
fn words_after_double_dash_are_not_options_on_zsh() {
    require_zsh!();
    let out = regions_for("echo -n -- -n");
    assert!(
        out.contains("5 7 fg=cyan memo=zsh-pulse"),
        "first -n must be cyan: {out}"
    );
    assert!(
        !out.contains("11 13 fg=cyan"),
        "second -n after -- must not be cyan: {out}"
    );
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// A file and a directory that exist, for the path tests.
///
/// They are created under the temporary directory, so that the test does
/// not depend on what happens to be in the working directory.
fn existing_paths() -> (std::path::PathBuf, std::path::PathBuf) {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("zsh-pulse-path-test-{}-{count}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the test directory");
    let file = dir.join("a-file");
    std::fs::write(&file, b"a file\n").expect("write the test file");
    (file, dir)
}

#[test]
fn an_existing_file_is_magenta() {
    require_zsh!();
    let (file, _dir) = existing_paths();
    let path = file.display().to_string();
    let out = regions_for(&format!("cat {path}"));
    // The path starts after `cat ` and is as long as it is.
    let expected = format!(
        "REGIONS=<0 3 fg=green memo=zsh-pulse,4 {} fg=magenta memo=zsh-pulse>",
        4 + path.len()
    );
    assert!(
        out.contains(&expected),
        "an existing file must be magenta: {out}"
    );
}

#[test]
fn an_existing_directory_is_magenta_and_underlined() {
    require_zsh!();
    let (_file, dir) = existing_paths();
    let path = dir.display().to_string();
    let out = regions_for(&format!("cat {path}"));
    let expected = format!(
        "REGIONS=<0 3 fg=green memo=zsh-pulse,\
         4 {} fg=magenta,underline memo=zsh-pulse>",
        4 + path.len()
    );
    assert!(
        out.contains(&expected),
        "an existing directory must be magenta and underlined: {out}"
    );
}

#[test]
fn a_word_that_does_not_exist_is_not_a_path() {
    require_zsh!();
    // The word has a slash in it, so it *could* be a path — but there is
    // nothing there, so it keeps its ordinary style.
    let out = regions_for("cat ./no-such-file-here");
    assert!(
        out.contains("REGIONS=<0 3 fg=green memo=zsh-pulse>"),
        "a word that does not exist must not be highlighted: {out}"
    );
}

#[test]
fn a_tilde_is_the_home_directory() {
    require_zsh!();
    // `~` expands to the home directory, which is a directory: magenta and
    // underlined. The test needs a home directory to expand to.
    if std::env::var_os("HOME").is_none() {
        eprintln!(
            "SKIPPED: `HOME` is not set, so there is no home directory for \
             `~` to expand to."
        );
        return;
    }
    let out = regions_for("cat ~");
    assert!(
        out.contains(
            "REGIONS=<0 3 fg=green memo=zsh-pulse,\
             4 5 fg=magenta,underline memo=zsh-pulse>"
        ),
        "a tilde is the home directory, which is a directory: {out}"
    );
}

#[test]
fn the_command_inside_a_substitution_is_coloured() {
    require_zsh!();
    // `$(cmd)` holds a command of its own, and the module finds it: the
    // answer has to be about the word *inside* the substitution, not about
    // the whole `$(cmd)`.
    let out = regions_for("echo $(ls)");
    assert!(
        out.contains("0 4 fg=green memo=zsh-pulse"),
        "the outer command must be green: {out}"
    );
    assert!(
        out.contains("7 9 fg=green memo=zsh-pulse"),
        "the command inside the substitution must be green too: {out}"
    );
}

#[test]
fn an_argument_that_names_nothing_is_not_highlighted() {
    require_zsh!();
    // `hi` is looked up — every literal word is a candidate for naming a
    // file — but there is no file called `hi`, so it stays exactly as the
    // user typed it.
    let out = regions_for("echo hi");
    assert!(
        out.contains("REGIONS=<0 4 fg=green memo=zsh-pulse>"),
        "an argument that names nothing must not be highlighted: {out}"
    );
}

#[test]
fn a_bare_file_name_is_magenta() {
    require_zsh!();
    // A file name needs no slash: `Cargo.toml` names a file just as much
    // as `./Cargo.toml` does. The reference implementation looks every
    // word up (`research/upstream/fsh/fast-highlight:1265-1295`), and so
    // does this one.
    let dir = std::env::temp_dir().join(format!(
        "zsh-pulse-bare-name-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create the test directory");
    std::fs::write(dir.join("a-file"), b"a file\n").expect("write the test file");
    let out = regions_for_in(&dir, "cat a-file");
    assert!(
        out.contains("4 10 fg=magenta memo=zsh-pulse"),
        "a bare file name must be magenta: {out}"
    );
}

#[test]
fn a_bare_directory_name_is_magenta_and_underlined() {
    require_zsh!();
    let dir = std::env::temp_dir().join(format!(
        "zsh-pulse-bare-dir-test-{}",
        std::process::id()
    ));
    // The directory is removed first: a run that left it behind would
    // find the name already there, and the first half of this test would
    // fail.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the test directory");
    let out = regions_for_in(&dir, "ls a-dir");
    // The directory does not exist, so the name is left alone: the point
    // of this test is that the *word* is looked up, and a word that names
    // nothing is not coloured.
    assert!(
        !out.contains("3 8 fg=magenta"),
        "a name that names nothing must not be coloured: {out}"
    );
    std::fs::create_dir_all(dir.join("a-dir")).expect("create the test directory");
    let out = regions_for_in(&dir, "ls a-dir");
    assert!(
        out.contains("3 8 fg=magenta,underline memo=zsh-pulse"),
        "a bare directory name must be magenta and underlined: {out}"
    );
}

#[test]
fn an_append_assignment_does_not_hide_the_command() {
    require_zsh!();
    // `FOO+=bar cmd` appends to `FOO` and runs `cmd`. The Rust lexer
    // knows that an assignment may end in `+=`
    // (`crates/lexer/src/scanner.rs`, `looks_like_assignment`), and the
    // adapter has to know it too: if the two sides disagreed about what
    // an assignment is, the command word would never be answered and
    // would never be coloured.
    let out = regions_for("FOO+=bar ls");
    assert!(
        out.contains("9 11 fg=green memo=zsh-pulse"),
        "an append assignment must not hide the command word: {out}"
    );
}

#[test]
#[cfg(any(feature = "autocomplete", feature = "history", feature = "history-navigation"))]
fn complete_builtin_populates_matches_for_files() {
    require_zsh!();
    let (ok, out) = run_zsh(
        "BUFFER='ls Cargo' CURSOR=8 zsh-pulse-complete; print -rl -- \"${_zsh_pulse_matches[@]}\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("Cargo.toml"),
        "expected Cargo.toml in matches: {out}"
    );
}

#[test]
#[cfg(feature = "autocomplete")]
fn typeahead_line_cap_follows_the_terminal_and_buffer_height() {
    require_zsh!();
    // zsh-autocomplete's real-time list is capped by rendered lines, not by
    // matches: `min(16, LINES - BUFFERLINES - 1)`. The `(MORE)` marker and
    // each group heading count toward the cap. This test pins the formula;
    // the pty tests pin that the widget actually applies it.
    let adapter = adapter_path();
    let (ok, out) = run_zsh(
        &format!(
            "source {adapter}\n\
             for spec in '40 1' '10 1' '10 5' '3 1' '2 1'; do\n\
                 _zsh_pulse_typeahead_max_lines ${{=spec}}\n\
                 print -r -- \"$spec -> $?/$_ZSH_PULSE_MAX_LINES\"\n\
             done\n\
             ZSH_PULSE_TYPEAHEAD_LINES=4 _zsh_pulse_typeahead_max_lines 40 1\n\
             print -r -- \"override -> $?/$_ZSH_PULSE_MAX_LINES\"",
            adapter = adapter.display(),
        ),
        &[("ZSH_PULSE_AUTOSUGGEST", "0")],
    );
    assert!(ok, "zsh exited badly: {out}");
    for expected in [
        "40 1 -> 0/16",
        "10 1 -> 0/8",
        "10 5 -> 0/4",
        "3 1 -> 0/1",
        "2 1 -> 1/0",
        "override -> 0/4",
    ] {
        assert!(
            out.contains(expected),
            "expected `{expected}` in the line-cap results: {out}"
        );
    }
}

#[test]
#[cfg(any(feature = "autocomplete", feature = "history", feature = "history-navigation"))]
fn complete_builtin_on_large_dir_does_not_hang() {
    require_zsh!();
    let store = std::path::Path::new("/nix/store");
    if !store.is_dir() {
        return;
    }
    let start = std::time::Instant::now();
    let (ok, out) = run_zsh(
        "BUFFER='ls /nix/store/m3y7' CURSOR=18 zsh-pulse-complete; print -rl -- \"${_zsh_pulse_matches[@]}\"",
        &[],
    );
    let elapsed = start.elapsed();
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "completion took too long: {elapsed:?}"
    );
    assert!(
        out.contains("m3y7"),
        "expected /nix/store match: {out}"
    );
}

#[test]
fn unclosed_parenthesis_is_highlighted_as_error() {
    require_zsh!();
    let out = regions_for("echo (foo");
    assert!(
        out.contains("5 6 fg=red,bold memo=zsh-pulse"),
        "unclosed parenthesis must be red,bold error: {out}"
    );
}

#[test]
fn unclosed_quote_is_highlighted_as_error() {
    require_zsh!();
    let out = regions_for("echo \"unclosed");
    assert!(
        out.contains("5 6 fg=red,bold memo=zsh-pulse"),
        "unclosed double quote must be red,bold error: {out}"
    );
}

#[test]
fn paired_brackets_highlighted_when_cursor_touches() {
    require_zsh!();
    let out = regions_for_with_cursor("echo (foo)", 10);
    assert!(
        out.contains("5 6 bg=blue memo=zsh-pulse"),
        "open bracket must be paired blue: {out}"
    );
    assert!(
        out.contains("9 10 bg=blue memo=zsh-pulse"),
        "close bracket must be paired blue: {out}"
    );
}

#[test]
fn comments_with_brackets_do_not_produce_bracket_error_on_zsh() {
    require_zsh!();
    let out = regions_for("echo hello # (this is comment");
    assert!(
        !out.contains("fg=red,bold"),
        "comment with bracket must not have red bracket error: {out}"
    );
    let out_quote = regions_for("echo hello # don't error");
    assert!(
        !out_quote.contains("fg=red,bold"),
        "comment with single quote must not have red quote error: {out_quote}"
    );
}

#[test]
fn brackets_inside_double_quotes_do_not_break_outer_brackets_on_zsh() {
    require_zsh!();
    let out = regions_for("echo ( \"foo ) bar\" )");
    assert!(
        !out.contains("fg=red,bold"),
        "parens enclosing double quote with paren must not error: {out}"
    );
}

#[test]
#[cfg(any(feature = "autocomplete", feature = "history", feature = "history-navigation"))]
fn complete_builtin_tilde_expansion_preserves_tilde_slash() {
    require_zsh!();

    let home = std::env::temp_dir().join(format!(
        "zsh-pulse-tilde-home-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::fs::create_dir_all(home.join("docs")).expect("create temp home contents");
    std::fs::write(home.join("note.txt"), b"hello").expect("create temp home file");
    let home_str = home.to_string_lossy().to_string();

    let (ok, out) = run_zsh(
        "BUFFER='ls ~'\n\
         CURSOR=4\n\
         zsh-pulse-complete --paths-only --max=5\n\
         print -r -- \"MATCHES=<${(j:,:)_zsh_pulse_matches}>\"",
        &[("HOME", home_str.as_str())],
    );

    let _ = std::fs::remove_dir_all(&home);

    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("MATCHES=<~/"),
        "expected tilde slash prefix preserved in matches: {out}"
    );
}

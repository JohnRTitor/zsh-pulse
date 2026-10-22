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
//! test skips with a printed reason instead of failing: shell-pulse is a
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
fn builtin_is_registered_under_its_exact_name() {
    require_zsh!();
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
    require_zsh!();
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
    require_zsh!();
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
    require_zsh!();
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
    require_zsh!();
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
    require_zsh!();
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
    require_zsh!();
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
    require_zsh!();
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
    require_zsh!();
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
        "export SHELL_PULSE_ECHO=1\n\
         for i in {1..25}; do\n\
             BUFFER=\"value $i\"\n\
             CURSOR=3\n\
             shell-pulse-highlight\n\
         done\n\
         # A command between the write and the read: this is what triggers\n\
         # zsh's pushheap/popheap pair and recycles pool memory.\n\
         :\n\
         print -r -- \"STATE=$SHELL_PULSE_STATE\"",
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
        "export SHELL_PULSE_ECHO=1\n\
         for i in {1..200}; do\n\
             BUFFER=\"value $i\"\n\
             CURSOR=1\n\
             shell-pulse-highlight\n\
         done\n\
         # `setaparam` always assigns an *array* (zsh-src/Src/params.c:3318),\n\
         # so the value is element 1 of a one-element array.\n\
         print -r -- \"VALUE=${SHELL_PULSE_STATE[1]}\"\n\
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
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         BUFFER='echo hi'\n\
         CURSOR=3\n\
         shell-pulse-highlight --region-highlight\n\
         print -r -- \"REGIONS=${(j:,:)region_highlight}\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    // `echo` is the command, characters 0 to 4. The argument is left alone.
    // The assertion is on the *whole* array, so a duplicate or a missing
    // region would fail it.
    assert!(
        out.contains("REGIONS=0 4 fg=cyan,bold memo=shell-pulse"),
        "unexpected regions: {out}"
    );
    assert!(
        !out.contains("fg=red") && !out.contains("fg=green"),
        "only the command should have been highlighted: {out}"
    );
}

#[test]
fn the_highlighter_reports_character_offsets_not_byte_offsets() {
    require_zsh!();
    // zsh's `region_highlight` uses the same units as `CURSOR`, which count
    // characters. A multi-byte character must not shift the offsets.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         BUFFER=$'echo \\u00e9 | grep x'\n\
         CURSOR=3\n\
         shell-pulse-highlight --region-highlight\n\
         print -r -- \"REGIONS=${(j:,:)region_highlight}\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    // `echo` is characters 0 to 4; the `|` operator is character 7 (it is
    // byte 8, because `é` takes two bytes). A byte-based implementation
    // would report 8.
    assert!(
        out.contains(
            "REGIONS=0 4 fg=cyan,bold memo=shell-pulse,7 8 fg=red memo=shell-pulse"
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
         shell-pulse-highlight --region-highlight\n\
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
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=(SENTINEL)\n\
         WIDGET=test-widget\n\
         BUFFER='echo hi'\n\
         CURSOR=3\n\
         shell-pulse-highlight\n\
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
         shell-pulse-highlight --region-highlight\n\
         region_highlight=(SENTINEL)\n\
         shell-pulse-highlight --region-highlight\n\
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
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         BUFFER='echo hi'\n\
         CURSOR=3\n\
         shell-pulse-highlight --region-highlight\n\
         region_highlight=(SENTINEL)\n\
         BUFFER='echo there'\n\
         CURSOR=3\n\
         shell-pulse-highlight --region-highlight\n\
         print -r -- \"REGIONS=${(j:,:)region_highlight}\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("REGIONS=0 4 fg=cyan,bold memo=shell-pulse"),
        "a changed line must be pushed again: {out}"
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
         shell-pulse-highlight --region-highlight\n\
         BUFFER=$'$(($(($(($(($(($(x)))))))))'\n\
         CURSOR=1\n\
         shell-pulse-highlight --region-highlight\n\
         print -r -- \"REGIONS=${(j:,:)region_highlight}\"\n\
         print -r -- SURVIVED",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(out.contains("SURVIVED"), "the shell did not finish: {out}");
    assert!(
        !out.contains("fg=cyan"),
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
             whence -w shell-pulse-highlight",
            mp = mp.display(),
            adapter = adapter.display(),
        ))
        .output()
        .expect("run zsh");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "sourcing the adapter failed: {text}");
    assert!(text.contains("ADAPTER_OK"), "the adapter broke the shell: {text}");
    assert!(
        text.contains("shell-pulse-highlight: builtin"),
        "the module was not loaded: {text}"
    );
}

#[test]
fn the_adapter_chains_an_existing_pre_redraw_hook() {
    require_zsh!();
    // zsh allows one widget per name, and `zle -N` replaces it. Other
    // plugins use `zle-line-pre-redraw` too, so the adapter must chain the
    // existing hook rather than destroy it.
    let adapter = adapter_path();
    let out = Command::new("zsh")
        .arg("-f")
        .arg("-c")
        .arg(format!(
            "zle -N zle-line-pre-redraw my-own-hook\n\
             source {adapter}\n\
             print -r -- \"WIDGET=${{widgets[zle-line-pre-redraw]}}\"\n\
             print -r -- \"PREVIOUS=$_shell_pulse_previous_pre_redraw\"",
            adapter = adapter.display(),
        ))
        .output()
        .expect("run zsh");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "sourcing the adapter failed: {text}");
    assert!(
        text.contains("WIDGET=user:shell-pulse-highlight-redraw"),
        "the adapter must install its own hook: {text}"
    );
    // `widgets[name]` is `user:<fn>`; the adapter has to strip the prefix
    // before it can call the function, so what it remembers is the bare
    // name.
    assert!(
        text.contains("PREVIOUS=my-own-hook"),
        "the adapter must remember the hook it replaced, callable: {text}"
    );
}

#[test]
fn the_adapters_default_module_path_is_where_the_installer_puts_the_module() {
    require_zsh!();
    // `bench/install.sh` installs into `bench/mp`; the adapter must look
    // there by default, or the plugin silently does nothing.
    let adapter = adapter_path();
    let out = Command::new("zsh")
        .arg("-f")
        .arg("-c")
        .arg(format!(
            "source {adapter}\n\
             print -r -- \"DEFAULT=$SHELL_PULSE_MODULE_PATH\"",
            adapter = adapter.display(),
        ))
        .output()
        .expect("run zsh");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "sourcing the adapter failed: {text}");
    assert!(
        text.contains("DEFAULT=") && text.contains("bench/mp"),
        "the default module path must match the installer: {text}"
    );
}

#[test]
fn the_array_write_path_survives_many_writes() {
    // The production write path is `set_param_array`, not the scalar path.
    // The failure it must survive — zsh freeing memory it was given — only
    // shows up when the *same* parameter is written over and over, so the
    // test writes it many times.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         for i in {1..200}; do\n\
             BUFFER=\"value $i\"\n\
             CURSOR=3\n\
             shell-pulse-highlight --region-highlight\n\
         done\n\
         :\n\
         print -r -- \"N=${#region_highlight} R=${(j:,:)region_highlight}\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("N=1 R=0 5 fg=cyan,bold memo=shell-pulse"),
        "the last write must be the one that survives: {out}"
    );
}

#[test]
fn a_line_that_is_too_long_is_not_highlighted() {
    // A pasted megabyte must not be highlighted, and the highlights of the
    // previous line must not be left on screen.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         BUFFER='echo hi'\n\
         CURSOR=3\n\
         shell-pulse-highlight --region-highlight\n\
         BUFFER=${(l:20000::x:)}\n\
         CURSOR=1\n\
         shell-pulse-highlight --region-highlight\n\
         print -r -- \"N=${#region_highlight}\"",
        &[],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert!(
        out.contains("N=0"),
        "a line over the limit must not be highlighted, and the old regions \
         must be cleared: {out}"
    );
}

#[test]
fn exactly_one_write_happens_per_change() {
    // "One `setaparam` per change" is a requirement, and counting the
    // module's own diagnostics is the only way to observe it.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         for i in 1 2 3; do\n\
             # The command word changes length, so the styling changes too.\n\
             BUFFER=\"echo$(print -r -- {1..$i}.x)\"\n\
             CURSOR=3\n\
             shell-pulse-highlight --region-highlight\n\
         done\n\
         print -r -- \"DONE\"",
        &[("SHELL_PULSE_DEBUG", "1")],
    );
    assert!(ok, "zsh exited badly: {out}");
    let published = out.matches("published ").count();
    assert_eq!(
        published, 3,
        "three changes must produce three writes, not {published}: {out}"
    );
}

#[test]
fn an_unchanged_line_is_not_written_again() {
    // The counterpart: a redraw that does not change the line must not
    // write at all.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         BUFFER='echo hi'\n\
         CURSOR=3\n\
         shell-pulse-highlight --region-highlight\n\
         shell-pulse-highlight --region-highlight\n\
         print -r -- \"DONE\"",
        &[("SHELL_PULSE_DEBUG", "1")],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert_eq!(
        out.matches("published ").count(),
        1,
        "the second, identical redraw must not write again: {out}"
    );
}

#[test]
fn a_new_line_makes_the_module_publish_again() {
    // zsh empties the display array at the end of every line
    // (`zsh-src/Src/Zle/zle_main.c:1398`). The module is told that a new
    // line is starting with `--reset`, and must then publish again even
    // though the text has not changed — otherwise a recalled command would
    // be drawn with no highlighting at all.
    let (ok, out) = run_zsh(
        "typeset -ga region_highlight\n\
         region_highlight=()\n\
         WIDGET=test-widget\n\
         BUFFER='echo hi'\n\
         CURSOR=3\n\
         shell-pulse-highlight --region-highlight\n\
         shell-pulse-highlight --reset\n\
         shell-pulse-highlight --region-highlight\n\
         print -r -- \"DONE\"",
        &[("SHELL_PULSE_DEBUG", "1")],
    );
    assert!(ok, "zsh exited badly: {out}");
    assert_eq!(
        out.matches("published ").count(),
        2,
        "a new line must be published again: {out}"
    );
}

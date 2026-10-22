//! The interactive test: a real zsh, a real terminal, real bytes.
//!
//! Everything else in this crate's test suite proves that the module
//! *computes* the right regions. This file proves that the regions reach
//! the user's screen: it drives an interactive zsh under a pseudo-terminal,
//! types a line into its line editor and asserts on the bytes zsh writes
//! to the terminal.
//!
//! Asserting on shell variables would prove nothing: `region_highlight`
//! can be filled without a single byte ever being drawn. The terminal
//! output is the only place where "the highlighting works" and "the
//! highlighting was computed" come apart.
//!
//! Run with: `cargo test -p zsh-module --test zsh_pty`
//!
//! # Skipping
//!
//! The harness needs three things a machine may not have: a `zsh` with the
//! optional `zsh/zpty` module, a pseudo-terminal, and a terminal type with
//! colour capabilities. Each of those is a *test dependency*, not a
//! property of the plugin, so each one is a documented skip with a printed
//! reason rather than a failure. What must never happen is a silent pass:
//! every skip says why it happened.

mod common;

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use common::{adapter_path, install_module, require_zsh};

/// The prompt the shell under the pty draws.
///
/// Plain text with no escape sequences: every assertion in this file is
/// about the bytes the *highlighter* produces, and a decorated prompt
/// would put colour of its own on the screen.
const PROMPT: &str = "SPPROMPT> ";

/// What the shell under the pty prints when the module really loaded.
///
/// Without it the "nothing is highlighted" assertions would be vacuous:
/// a broken setup looks exactly like a highlighter that does nothing.
const MODULE_LOADED: &str = "SP_MODULE_LOADED";

/// The argument typed after the command word.
///
/// An ordinary argument is not highlighted, so this text is what proves
/// that the highlight stops at the end of the command.
const ARGUMENT: &str = "SPARGXYZ";

/// The bytes zsh writes for the `fg=cyan,bold` region of the command word.
///
/// **Measured, not assumed.** The bytes zsh writes for an attribute come
/// from the terminal's capabilities, so the exact sequence depends on
/// `TERM`. On the terminals this test accepts (see [`colour_terminal`])
/// those capabilities are the standard xterm ones, and zsh writes the
/// attributes first and the foreground colour second: SGR 1 is bold and
/// SGR 36 is cyan, so `fg=cyan,bold` becomes `ESC [ 1 m` `ESC [ 3 6 m`.
///
/// The `zsh_pty` test binary dumps the raw bytes it captured, so this
/// constant can be checked against reality rather than against a guess.
const CYAN_BOLD: &[u8] = b"\x1b[1m\x1b[36m";

/// The bytes zsh writes for the cyan foreground colour on its own.
const CYAN: &[u8] = b"\x1b[36m";

/// The bytes zsh writes to put the foreground colour back to the default.
///
/// This is what closes a highlighted region. SGR 39 is "default
/// foreground", and zsh writes it when it leaves the region — on every
/// terminal this test accepts. (Some terminals also get a full reset,
/// `ESC [ 0 m`, but the `linux` terminfo does not, so this sequence is
/// the one that can be relied on.)
const DEFAULT_FOREGROUND: &[u8] = b"\x1b[39m";

/// The zsh harness that drives the pseudo-terminal.
///
/// It is written to a temporary directory and run with `zsh -f`, so it
/// never sees the user's configuration. Everything it needs comes from the
/// environment:
///
/// | variable | meaning |
/// |---|---|
/// | `SP_READY_FILE` | file holding the bytes that mean "the shell is up" |
/// | `SP_DONE_FILE` | file holding the bytes to wait for after typing |
/// | `SP_DONE_COUNT` | how many times those bytes must appear |
/// | `SP_TYPE` | the text to type into the line editor |
/// | `SP_OUT` | file to write the captured bytes to |
/// | `SP_TIMEOUT` | seconds to wait before giving up |
///
/// The shell under the pty reads its configuration from `$ZDOTDIR`, so the
/// caller decides what it loads — with the module, or without it.
const HARNESS: &str = r#"
# shell-pulse — the interactive pty harness.
#
# Starts an interactive zsh under a pseudo-terminal, types a line into its
# line editor and records every byte the shell writes to the terminal.
#
# The bytes are the only thing that proves the highlighting reached the
# screen: a shell variable could be set without anything ever being drawn.

emulate -L zsh
setopt no_unset

zmodload zsh/zpty 2>/dev/null || {
  print -u2 "the zsh/zpty module is not available in this zsh"
  exit 3
}

typeset -g READY=$(<${SP_READY_FILE})
typeset -g DONE=$(<${SP_DONE_FILE})
typeset -g COUNT=${SP_DONE_COUNT:-1}
typeset -g TYPED=${SP_TYPE-}
typeset -g OUT=${SP_OUT-}
typeset -g TIMEOUT=${SP_TIMEOUT:-10}
typeset -g PTY=shellpulse-pty
typeset -g CAP=''
typeset -gi OCCURRENCES=0

# The pty must not outlive this script: `zpty -d` hangs up the shell.
trap 'zpty -d $PTY 2>/dev/null' EXIT INT TERM

# Count the occurrences of $2 in $1.
#
# The needle is compared literally: the bytes we look for contain `[`,
# which would otherwise be read as the start of a character class.
count_occurrences() {
  local hay=$1 needle=$2
  OCCURRENCES=0
  while [[ $hay == *"$needle"* ]]; do
    (( OCCURRENCES++ ))
    hay=${hay#*"$needle"}
  done
  return 0
}

if ! zpty -b $PTY zsh -i; then
  print -u2 "this environment cannot provide a pseudo-terminal"
  exit 3
fi

# Phase one: wait for the shell to draw its prompt.
#
# The prompt is the signal that the terminal is in raw mode and the line
# editor is running. Typing before that would be swallowed by the tty
# driver's canonical-mode line discipline, which holds input back until a
# newline arrives.
typeset -i deadline=$(( SECONDS + TIMEOUT ))
typeset -i ready=0
while (( SECONDS < deadline )); do
  typeset chunk=''
  if zpty -rt $PTY chunk 2>/dev/null; then
    CAP+=$chunk
  fi
  if [[ $CAP == *"$READY"* ]]; then
    ready=1
    break
  fi
  sleep 0.02
done

# Phase two: type the line, then wait for the bytes we expect.
typeset -i typed=0
if (( ready )); then
  zpty -wn $PTY "$TYPED"
  typed=1
fi

deadline=$(( SECONDS + TIMEOUT ))
while (( SECONDS < deadline )); do
  typeset chunk=''
  if zpty -rt $PTY chunk 2>/dev/null; then
    CAP+=$chunk
  fi
  count_occurrences $CAP $DONE
  (( OCCURRENCES >= COUNT )) && break
  sleep 0.02
done

print -rn -- "$CAP" > $OUT
print -r -- "ready=$ready typed=$typed seen=$OCCURRENCES bytes=${#CAP}"
"#;

/// One run of the harness.
struct Capture {
    /// Every byte the shell under the pty wrote to the terminal.
    bytes: Vec<u8>,
    /// The harness's own status line, for the failure message.
    status: String,
}

impl Capture {
    /// Whether the captured bytes contain `needle`.
    fn contains(&self, needle: &[u8]) -> bool {
        self.bytes.windows(needle.len()).any(|window| window == needle)
    }

    /// Every offset at which `needle` occurs.
    fn occurrences(&self, needle: &[u8]) -> Vec<usize> {
        self.bytes
            .windows(needle.len())
            .enumerate()
            .filter(|(_, window)| *window == needle)
            .map(|(at, _)| at)
            .collect()
    }

    /// The captured bytes, escaped so that a failure message is readable.
    fn dump(&self) -> String {
        String::from_utf8_lossy(&self.bytes).escape_debug().to_string()
    }
}

/// A scenario to run under the pseudo-terminal.
struct Scenario {
    /// The text typed into the line editor.
    typed: &'static str,
    /// The bytes the harness waits for after typing.
    wait_for: &'static [u8],
    /// Whether the shell under the pty loads the module.
    load_module: bool,
}

/// Kills the harness — and with it the pseudo-terminal — when the test
/// unwinds.
///
/// Without this a failing assertion would leave an interactive zsh running
/// under a pty, holding on to the test's terminal.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Wait for `child`, giving up after `limit`.
///
/// A harness that hangs would hang the whole test run, so the wait is
/// bounded. The caller turns the timeout into a failure: the pty is
/// supposed to be under our control.
fn wait_with_timeout(child: &mut Child, limit: Duration) -> std::io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A fresh temporary directory for one scenario.
///
/// The name carries the process id and the scenario's label, so two tests
/// running in parallel never share one.
fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shell-pulse-pty-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the temporary directory");
    dir
}

/// Write the configuration the shell under the pty starts with.
///
/// `ZDOTDIR` points at this directory, so the interactive shell reads
/// `.zshrc` from here and from nowhere else: the user's own configuration
/// cannot influence the result.
fn write_child_rc(dir: &Path, load_module: bool) {
    let mut rc = String::new();
    rc.push_str(&format!("PS1='{PROMPT}'\n"));
    rc.push_str("RPROMPT=\n");
    rc.push_str("PROMPT_EOL_MARK=''\n");
    if load_module {
        // The adapter looks for the module here, exactly as a user who
        // ran `bench/install.sh` would have it.
        rc.push_str(&format!(
            "export SHELL_PULSE_MODULE_PATH='{}'\n",
            install_module().display()
        ));
        rc.push_str(&format!("source '{}'\n", adapter_path().display()));
    }
    // Proof that the module really loaded. Without it, "nothing was
    // highlighted" would prove nothing at all.
    rc.push_str(&format!(
        "if (( ${{+builtins[shell-pulse-highlight]}} )); then print -r -- {MODULE_LOADED}; fi\n"
    ));
    fs::write(dir.join(".zshrc"), rc).expect("write the child's .zshrc");
}

/// Run one scenario under a pseudo-terminal.
///
/// Returns `None` when this environment cannot provide a pseudo-terminal.
/// That is a documented skip, not a failure: the pseudo-terminal is a test
/// dependency, and the plugin has no say in whether the machine has one.
fn capture(scenario: &Scenario, term: &str, label: &str) -> Option<Capture> {
    let dir = temp_dir(label);

    let harness = dir.join("harness.zsh");
    fs::write(&harness, HARNESS).expect("write the pty harness");
    let ready = dir.join("ready.bin");
    fs::write(&ready, PROMPT.as_bytes()).expect("write the ready marker");
    let done = dir.join("done.bin");
    fs::write(&done, scenario.wait_for).expect("write the wait marker");
    let out = dir.join("capture.bin");

    let rc_dir = dir.join("rc");
    fs::create_dir_all(&rc_dir).expect("create the rc directory");
    write_child_rc(&rc_dir, scenario.load_module);

    let child = Command::new("zsh")
        .arg("-f")
        .arg(&harness)
        .env("SP_READY_FILE", &ready)
        .env("SP_DONE_FILE", &done)
        .env("SP_DONE_COUNT", "1")
        .env("SP_TYPE", scenario.typed)
        .env("SP_OUT", &out)
        .env("SP_TIMEOUT", "10")
        .env("ZDOTDIR", &rc_dir)
        .env("TERM", term)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the pty harness");
    let mut guard = ChildGuard(child);

    let status = wait_with_timeout(&mut guard.0, Duration::from_secs(30))
        .expect("wait for the pty harness")
        .unwrap_or_else(|| {
            panic!("the pty harness did not finish within 30 s: the pseudo-terminal hung")
        });

    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = guard.0.stdout.take() {
        pipe.read_to_string(&mut stdout).expect("read the harness output");
    }
    if let Some(mut pipe) = guard.0.stderr.take() {
        pipe.read_to_string(&mut stderr).expect("read the harness diagnostics");
    }

    if status.code() == Some(3) {
        eprintln!("SKIPPED: {}", stderr.trim());
        return None;
    }

    assert!(
        status.success(),
        "the pty harness failed ({status})\nstdout: {stdout}\nstderr: {stderr}"
    );

    let bytes = fs::read(&out).expect("read the captured bytes");
    // The scratch directory is not needed any more; the bytes it held are
    // in memory now.
    let _ = fs::remove_dir_all(&dir);
    Some(Capture {
        bytes,
        status: stdout.trim().to_string(),
    })
}

/// The bytes zsh writes for a prompt escape under `term`.
///
/// zsh's prompt escapes are the same machinery the line editor uses to
/// turn an attribute into terminal bytes, so they say what the terminal
/// supports.
fn prompt_escape(term: &str, escape: &str) -> Vec<u8> {
    let out = Command::new("zsh")
        .arg("-f")
        .arg("-c")
        .arg(format!("print -Pn '{escape}'"))
        .env("TERM", term)
        .output()
        .expect("run zsh");
    out.stdout
}

/// The `TERM` to run the pseudo-terminal under.
///
/// The bytes zsh writes for an attribute come from the terminal's
/// capabilities, so the exact sequence depends on `TERM`. The test pins
/// `TERM` to a terminal whose capabilities are the standard xterm ones and
/// checks that with zsh itself before using it: `%B` is the bold sequence
/// and `%F{cyan}` the foreground-colour one. Asserting on bytes this test
/// did not measure would be a guess, and a terminal that answers
/// differently would make the guess wrong.
fn colour_terminal() -> Option<&'static str> {
    const CANDIDATES: &[&str] = &[
        "xterm-256color",
        "xterm",
        "screen",
        "linux",
        "xterm-kitty",
    ];
    CANDIDATES.iter().copied().find(|term| {
        prompt_escape(term, "%B") == b"\x1b[1m" && prompt_escape(term, "%F{cyan}") == b"\x1b[36m"
    })
}

#[test]
fn the_typed_command_is_highlighted_on_the_terminal() {
    require_zsh!();
    let Some(term) = colour_terminal() else {
        eprintln!(
            "SKIPPED: no terminal type with the standard xterm colour capabilities \
             was found, so the bytes zsh writes for `fg=cyan,bold` cannot be known \
             in advance and asserting on them would be a guess."
        );
        return;
    };
    let Some(capture) = capture(
        &Scenario {
            typed: "echo SPARGXYZ",
            wait_for: ARGUMENT.as_bytes(),
            load_module: true,
        },
        term,
        "highlighted",
    ) else {
        // `capture` printed the reason it skipped.
        return;
    };

    // The module really loaded: without this the whole scenario would be
    // testing a shell that never had the plugin in it.
    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load in the shell under the pty, so nothing could \
         have been highlighted.\n{}",
        capture.dump()
    );

    assert!(
        capture.contains(CYAN_BOLD),
        "the cyan+bold region of the command word never reached the terminal.\n\
         expected the bytes {CYAN_BOLD:?} somewhere in the terminal output.\n\
         harness: {}\ncaptured {} bytes:\n{}",
        capture.status,
        capture.bytes.len(),
        capture.dump()
    );

    // The region has to be closed again, or the rest of the line would be
    // drawn in the command's colour.
    assert!(
        capture.contains(DEFAULT_FOREGROUND),
        "the highlighted region was never closed: expected the bytes \
         {DEFAULT_FOREGROUND:?}.\n{}",
        capture.dump()
    );

    // The argument was typed and drawn.
    let drawn = capture.occurrences(ARGUMENT.as_bytes());
    assert!(
        !drawn.is_empty(),
        "the argument {ARGUMENT} was never drawn, so the line was not edited \
         at all.\n{}",
        capture.dump()
    );

    // The highlight stops at the end of the command word: an ordinary
    // argument is not styled, so the bytes in front of the argument cannot
    // be the cyan sequence.
    for at in drawn {
        let before = &capture.bytes[at.saturating_sub(CYAN_BOLD.len())..at];
        assert_ne!(
            before,
            CYAN_BOLD,
            "the argument {ARGUMENT} was drawn inside the highlighted region: \
             an ordinary argument must not be styled.\n{}",
            capture.dump()
        );
    }
}

#[test]
fn an_empty_line_is_not_highlighted() {
    require_zsh!();
    let Some(term) = colour_terminal() else {
        eprintln!(
            "SKIPPED: no terminal type with the standard xterm colour \
             capabilities was found, so there is nothing to compare against."
        );
        return;
    };
    let Some(capture) = capture(
        &Scenario {
            typed: "",
            wait_for: PROMPT.as_bytes(),
            load_module: true,
        },
        term,
        "empty",
    ) else {
        // `capture` printed the reason it skipped.
        return;
    };

    // The module really loaded, so the absence of colour below is the
    // highlighter's decision and not a broken setup.
    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load in the shell under the pty.\n{}",
        capture.dump()
    );
    assert!(
        !capture.contains(CYAN),
        "an empty line has no styled region, so nothing may be drawn in \
         colour.\ncaptured {} bytes:\n{}",
        capture.bytes.len(),
        capture.dump()
    );
}

#[test]
fn without_the_module_nothing_is_highlighted() {
    require_zsh!();
    let Some(term) = colour_terminal() else {
        eprintln!(
            "SKIPPED: no terminal type with the standard xterm colour \
             capabilities was found, so there is nothing to compare against."
        );
        return;
    };
    let Some(capture) = capture(
        &Scenario {
            typed: "echo SPARGXYZ",
            wait_for: ARGUMENT.as_bytes(),
            load_module: false,
        },
        term,
        "no-module",
    ) else {
        // `capture` printed the reason it skipped.
        return;
    };

    // The control: zsh itself must not colour the line, or the assertion
    // in the first test would prove nothing about shell-pulse.
    assert!(
        !capture.contains(MODULE_LOADED.as_bytes()),
        "the module was loaded although this scenario asks for a shell \
         without it.\n{}",
        capture.dump()
    );
    assert!(
        capture.contains(ARGUMENT.as_bytes()),
        "the line was never drawn, so there is nothing to compare against.\n{}",
        capture.dump()
    );
    assert!(
        !capture.contains(CYAN),
        "zsh itself must not colour the command line; if it does, the other \
         test in this file cannot tell shell-pulse's highlighting apart from \
         zsh's own.\ncaptured {} bytes:\n{}",
        capture.bytes.len(),
        capture.dump()
    );
}

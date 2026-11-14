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
//! # The colours
//!
//! The plugin publishes these attributes (the table is in
//! `crates/zsh-module/src/region.rs`):
//!
//! | what | attribute | the bytes zsh writes |
//! |---|---|---|
//! | a command the shell knows | `fg=green` | `ESC [ 3 2 m` |
//! | a command the shell does not know | `fg=red,bold` | `ESC [ 1 m` `ESC [ 3 1 m` |
//! | a command-line option (`-l`, `--amend`) | `fg=cyan` | `ESC [ 3 6 m` |
//! | a word naming an existing directory | `fg=magenta,underline` | `ESC [ 4 m` `ESC [ 3 5 m` |
//! | a word naming an existing file | `fg=magenta` | `ESC [ 3 5 m` |
//!
//! zsh writes the **attributes first and the foreground colour second**,
//! and the bytes themselves come from the terminal's capabilities — so
//! they are measured here, not assumed. See [`Palette`].
//!
//! # Skipping
//!
//! The harness needs three things a machine may not have: a `zsh` with the
//! optional `zsh/zpty` module, a pseudo-terminal, and a terminal type with
//! colour capabilities. Each of those is a *test dependency*, not a
//! property of the plugin, so each one is a documented skip with a printed
//! reason rather than a failure. What must never happen is a silent pass:
//! every skip says why it happened.
//!
//! Every skip prints two things on stderr: the reason in prose, and the
//! marker `SKIP-PTY: <reason>`. The marker is what a machine can read.
//! **A CI job that requires this test must grep the output for `SKIP-PTY`
//! and fail when it finds one**, because a skipped end-to-end test must
//! never be indistinguishable from a passing one.

mod common;

use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use common::{adapter_path, install_module, require_zsh};

/// The marker that says "this test did not run".
///
/// A CI job that *requires* the pty test must grep the output for
/// `SKIP-PTY` and fail when it finds one: a skipped end-to-end test must
/// never be indistinguishable from a passing one.
const SKIP_MARKER: &str = "SKIP-PTY";

/// Prints the machine-readable skip marker when it goes out of scope.
///
/// The reason is printed in prose by whoever detects the missing
/// dependency; the marker is printed by this guard, so that the two
/// cannot drift apart and so that a test which returns early cannot
/// forget the marker.
struct SkipMarker(&'static str);

impl Drop for SkipMarker {
    fn drop(&mut self) {
        eprintln!("{SKIP_MARKER}: {}", self.0);
    }
}

/// Print the reason a test did not run.
///
/// Two things go to stderr: the reason in prose, and the machine-readable
/// marker. The marker is printed by the *test*, not only by the helpers
/// that detect the missing dependency, so `cargo test -- --nocapture`
/// shows it next to the test that skipped.
fn skip(reason: &str) {
    eprintln!("SKIPPED: {reason}");
    eprintln!("{SKIP_MARKER}: {reason}");
}

/// Skip the calling test when there is no `zsh` to test against.
///
/// `zsh` is a test dependency, not a property of the plugin: without it
/// there is no shell to load the module into, so the assertion could not
/// be made at all (`common::zsh_available` says why skipping is the right
/// answer rather than failing).
///
/// The shared `require_zsh!` macro prints the human-readable reason and
/// returns; the guard prints the machine-readable marker as the test
/// leaves, so that both belong to the same skip.
macro_rules! require_zsh_pty {
    () => {
        if !common::zsh_available() {
            // The marker is printed when this scope is left — that is,
            // after `require_zsh!` has printed the reason.
            let _marker = SkipMarker("`zsh` is not on PATH");
            require_zsh!();
        }
    };
}

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
///
/// It is also the only part of the line that reaches the terminal as
/// *contiguous* bytes: zsh writes the attribute of a styled word before
/// every one of its characters, so a styled word cannot be searched for
/// in the capture.
const ARGUMENT: &str = "SPARGXYZ";

/// The argument typed after the command word of the *second* line.
///
/// A different word on purpose. With the same marker on both lines the
/// harness's wait would be satisfied by the first line alone — the
/// marker appears once as the drawn argument and once as the output of
/// the `echo` — and the second line would never have to be drawn.
const SECOND_ARGUMENT: &str = "SPARGZULU";

/// The bytes zsh writes for the attributes zsh-pulse publishes.
///
/// **Measured, not assumed.** The bytes zsh writes for an attribute come
/// from the terminal's capabilities, so the exact sequence depends on
/// `TERM`. zsh's prompt escapes are the same machinery the line editor
/// uses to turn an attribute into terminal bytes, so they say what the
/// terminal supports: `%B` is the bold sequence, `%U` the underline one,
/// `%F{green}` the foreground-colour one and `%f` the way back to the
/// default foreground.
///
/// zsh writes the **attributes first and the foreground colour second**,
/// so `fg=red,bold` becomes `ESC [ 1 m` `ESC [ 3 1 m` and
/// `fg=magenta,underline` becomes `ESC [ 4 m` `ESC [ 3 5 m`.
///
/// The `zsh_pty` test binary dumps the raw bytes it captured, so these
/// can be checked against reality rather than against a guess.
struct Palette {
    /// SGR 1: bold.
    bold: Vec<u8>,
    /// SGR 4: underline.
    underline: Vec<u8>,
    /// SGR 31: the red foreground.
    red: Vec<u8>,
    /// SGR 32: the green foreground.
    green: Vec<u8>,
    /// SGR 33: the yellow foreground.
    yellow: Vec<u8>,
    /// SGR 34: the blue foreground.
    blue: Vec<u8>,
    /// SGR 35: the magenta foreground.
    magenta: Vec<u8>,
    /// SGR 36: the cyan foreground.
    cyan: Vec<u8>,
    /// SGR 39: the default foreground — what closes a region.
    ///
    /// This is what closes a highlighted region. SGR 39 is "default
    /// foreground", and zsh writes it when it leaves the region — on every
    /// terminal this test accepts. (Some terminals also get a full reset,
    /// `ESC [ 0 m`, but the `linux` terminfo does not, so this sequence is
    /// the one that can be relied on.)
    default_foreground: Vec<u8>,
    /// The bytes for `fg=red,bold`: the attribute, then the colour.
    red_bold: Vec<u8>,
    /// The bytes for `fg=magenta,underline`: the attribute, then the colour.
    magenta_underline: Vec<u8>,
}

impl Palette {
    /// Measure the sequences zsh writes under `term`.
    ///
    /// Every sequence is asked of zsh itself, through the prompt escape
    /// that stands for it. Nothing here is written down by hand: a
    /// terminal that answers differently would answer differently here
    /// too, and the assertions would follow it.
    fn measure(term: &str) -> Palette {
        let bold = prompt_escape(term, "%B");
        let underline = prompt_escape(term, "%U");
        let red = prompt_escape(term, "%F{red}");
        let green = prompt_escape(term, "%F{green}");
        let yellow = prompt_escape(term, "%F{yellow}");
        let blue = prompt_escape(term, "%F{blue}");
        let magenta = prompt_escape(term, "%F{magenta}");
        let cyan = prompt_escape(term, "%F{cyan}");
        let default_foreground = prompt_escape(term, "%f");
        Palette {
            // The attributes come first and the foreground colour second:
            // this is the order zsh writes them in, and it is the order
            // `fg=red,bold` and `fg=magenta,underline` are read in.
            red_bold: [bold.as_slice(), red.as_slice()].concat(),
            magenta_underline: [underline.as_slice(), magenta.as_slice()].concat(),
            bold,
            underline,
            red,
            green,
            yellow,
            blue,
            magenta,
            cyan,
            default_foreground,
        }
    }

    /// Whether the terminal answers with the standard xterm sequences.
    ///
    /// SGR 1 is bold, SGR 4 is underline, SGR 31, 32, 33, 34, 35 and 36
    /// are red, green, yellow, blue, magenta and cyan, and SGR 39 is the
    /// default foreground. The test pins `TERM` to a terminal with these
    /// capabilities, so that the bytes it asserts on are the ones in the
    /// table at the top of this file.
    fn is_standard_xterm(&self) -> bool {
        self.bold == b"\x1b[1m".as_slice()
            && self.underline == b"\x1b[4m".as_slice()
            && self.red == b"\x1b[31m".as_slice()
            && self.green == b"\x1b[32m".as_slice()
            && self.yellow == b"\x1b[33m".as_slice()
            && self.blue == b"\x1b[34m".as_slice()
            && self.magenta == b"\x1b[35m".as_slice()
            && self.cyan == b"\x1b[36m".as_slice()
            && self.default_foreground == b"\x1b[39m".as_slice()
    }

    /// Every sequence the plugin can write.
    ///
    /// The plugin's styles are a colour, an attribute, or both
    /// (`crates/zsh-module/src/region.rs`), so "nothing was coloured" has
    /// to be checked against all of them: the six colours it publishes,
    /// the two attributes, and the two combinations. Checking only one of
    /// them would let the others through.
    fn every_sequence(&self) -> Vec<&[u8]> {
        vec![
            self.bold.as_slice(),
            self.underline.as_slice(),
            self.red.as_slice(),
            self.green.as_slice(),
            self.yellow.as_slice(),
            self.blue.as_slice(),
            self.magenta.as_slice(),
            self.cyan.as_slice(),
            self.red_bold.as_slice(),
            self.magenta_underline.as_slice(),
        ]
    }
}

/// A terminal the assertions may be made about.
///
/// The `TERM` name and the bytes zsh writes for the plugin's attributes
/// belong together: the bytes are measured from that terminal's
/// capabilities, so they may only be used with that terminal.
struct Terminal {
    /// The `TERM` to run the pseudo-terminal under.
    name: &'static str,
    /// The bytes zsh writes for the attributes zsh-pulse publishes.
    palette: Palette,
}

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
# zsh-pulse — the interactive pty harness.
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
typeset -g KEYS=${SP_KEYS_FILE-}
typeset -g SNAPSHOTS=${SP_SNAPSHOTS-}
typeset -g OUT=${SP_OUT-}
typeset -g TIMEOUT=${SP_TIMEOUT:-10}
typeset -g PTY=zshpulse-pty
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

# Phase two: type the line(s), then wait for the bytes we expect.
#
# A newline in SP_TYPE starts a new command line: the text before it is
# submitted and the rest is typed into the next one. Typing more than one
# line matters -- a plugin that works on the first line and then goes quiet
# looks exactly like one that works.
typeset -i typed=0
if (( ready )) && [[ -n $KEYS ]]; then
  # Paced key mode. The keys are separated by ASCII unit separator (0x1f),
  # not by newline: a key may itself be Enter.
  #
  # The input has to be paced. zsh-pulse skips the typeahead preview while
  # `KEYS_QUEUED_COUNT` is non-zero, so writing a whole line at once would
  # suppress the very previews these tests exist to observe. After each key
  # the harness reads until the terminal has been quiet for a few ticks,
  # then records the capture length: that offset is the snapshot the test
  # inspects for this key's output.
  typeset -g rest=$(<${KEYS})
  # The file ends with a sentinel byte, not a newline. Without it, command
  # substitution would strip a trailing Enter key: the key *is* a newline.
  rest=${rest%$'\x1e'}
  typeset -a keys=()
  while [[ -n $rest ]]; do
    keys+=("${rest%%$'\x1f'*}")
    if [[ $rest == *$'\x1f'* ]]; then
      rest=${rest#*$'\x1f'}
    else
      rest=''
    fi
  done

  [[ -n $SNAPSHOTS ]] && : > $SNAPSHOTS
  typeset -i ki quiet ticks
  for ki in {1..${#keys}}; do
    zpty -wn $PTY "$keys[ki]"
    quiet=0
    ticks=0
    # Wait for output, and for it to stop again. The quiet window is
    # deliberately longer than the gap before a synchronous completion
    # starts drawing; a shorter one would snapshot the previous screen and
    # attribute the next key's preview to the following key. The tick cap
    # is a safety net for a completion function that never stops writing.
    while (( quiet < 20 && ticks < 300 )); do
      typeset chunk=''
      if zpty -rt $PTY chunk 2>/dev/null; then
        CAP+=$chunk
        quiet=0
      else
        (( quiet++ ))
      fi
      (( ticks++ ))
      sleep 0.02
    done
    [[ -n $SNAPSHOTS ]] && print -r -- ${#CAP} >> $SNAPSHOTS
  done
  typed=1
elif (( ready )); then
  # The text is split on *newlines*, with the `f` flag of the `(@)`
  # parameter expansion flag.
  #
  # `${(@s:\\n:)TYPED}` would not do: inside double quotes `\\` is one
  # backslash, so that expansion splits on the two characters `\` and `n`
  # -- and a text without them is never split at all. The two command
  # lines would have been typed as one.
  typeset -a lines=("${(@f)TYPED}")
  integer li
  for li in {1..${#lines}}; do
    if (( li < ${#lines} )); then
      zpty -w $PTY "$lines[li]"$'\n'
    else
      zpty -wn $PTY "$lines[li]"
    fi
  done
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
    /// Capture lengths after each paced key, in order.
    ///
    /// Empty for runs that typed a whole line at once. For a paced run,
    /// `snapshots[i]` is where the output produced by key `i` ends, so
    /// `segment(i)` is exactly what that key drew.
    snapshots: Vec<usize>,
    /// The harness's own status line, for the failure message.
    status: String,
}

impl Capture {
    /// Whether the captured bytes contain `needle`.
    fn contains(&self, needle: &[u8]) -> bool {
        self.bytes
            .windows(needle.len())
            .any(|window| window == needle)
    }

    /// The bytes written while one paced key was processed.
    ///
    /// The segment runs from the end of the previous key's output to the
    /// end of this key's output. It is the snapshot that makes it possible
    /// to tell a preview drawn before Tab from the list drawn by Tab.
    fn segment(&self, index: usize) -> &[u8] {
        let start = if index == 0 {
            0
        } else {
            self.snapshots.get(index - 1).copied().unwrap_or(0)
        };
        let end = self
            .snapshots
            .get(index)
            .copied()
            .unwrap_or(self.bytes.len())
            .min(self.bytes.len());
        self.bytes.get(start..end).unwrap_or_default()
    }

    /// Whether one paced key's output contains `needle`.
    fn segment_contains(&self, index: usize, needle: &[u8]) -> bool {
        let segment = self.segment(index);
        segment.windows(needle.len()).any(|window| window == needle)
    }

    /// The captured bytes, escaped so that a failure message is readable.
    fn dump(&self) -> String {
        String::from_utf8_lossy(&self.bytes)
            .escape_debug()
            .to_string()
    }

    /// The screen the shell drew.
    ///
    /// The bytes on the wire are **not** the screen. zsh's line editor
    /// draws the line in pieces and moves the cursor between them, so the
    /// text of the line can be broken up — and even reordered — in the
    /// byte stream: what the user sees may be `echo SPARGXYZ` while the
    /// bytes are `SPARG` `ESC[10D` `echo` `ESC[6C` `X` `ESC[11D` `YZ`.
    ///
    /// Which of the two a test looks at decides whether it is reliable.
    /// A search for a piece of text in the raw bytes finds it only when
    /// zsh happened to draw it in one piece, which it does not always do:
    /// the capture depends on how the input happened to reach the shell.
    /// The screen is what the user sees, and it is the same every time.
    fn screen(&self) -> Screen {
        Screen::of(&self.bytes)
    }

    /// The positions in the screen at which `needle` appears.
    ///
    /// The positions are screen positions, so they can be handed to
    /// [`Capture::attrs_at`].
    fn screen_occurrences(&self, needle: &[u8]) -> Vec<usize> {
        self.screen().occurrences(needle)
    }

    /// The positions of the characters that were drawn with these
    /// attributes.
    fn screen_positions_with(&self, attrs: Attrs) -> Vec<usize> {
        self.screen().positions_with(attrs)
    }

    /// The attributes that were in force at a position of the screen.
    ///
    /// A region that was never closed would still be in force there.
    fn attrs_at(&self, position: usize) -> Option<Attrs> {
        self.screen().attrs_at(position)
    }
}

/// The screen a capture drew.
///
/// Only what zsh's line editor actually does is modelled: it writes text
/// at the cursor, moves the cursor, erases the line and sets the
/// attributes. Everything else in the byte stream is skipped.
struct Screen {
    /// The characters on the line.
    cells: Vec<Cell>,
}

/// One character of the screen.
struct Cell {
    /// The byte on the screen.
    byte: u8,
    /// The attributes that were in force when it was written.
    attrs: Attrs,
}

/// The attributes in force on the screen.
///
/// The escape sequences zsh writes are the terminal's own, and they are
/// parsed out of the bytes rather than written down: SGR 30 to 37 are the
/// foreground colours, 1 is bold, 4 is underline, 22 and 24 turn those
/// off again and 39 is the default foreground.
///
/// The *whole* set matters, not just the last sequence: `fg=red,bold` is
/// written as `ESC [ 1 m` `ESC [ 3 1 m`, and a character is red **and**
/// bold only because both were set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Attrs {
    /// The foreground colour, as the SGR number: 32 for green.
    fg: Option<u8>,
    /// Whether the text is bold.
    bold: bool,
    /// Whether the text is underlined.
    underline: bool,
}

impl Attrs {
    /// The attributes a run of escape sequences sets.
    ///
    /// The run is parsed the way the screen parses it, so the expected
    /// attributes of a region and the attributes the screen saw are
    /// computed the same way.
    fn of(bytes: &[u8]) -> Attrs {
        let mut attrs = Attrs::default();
        let mut at = 0;
        while at < bytes.len() {
            if bytes.get(at) != Some(&0x1b) || bytes.get(at + 1) != Some(&b'[') {
                at += 1;
                continue;
            }
            let mut end = at + 2;
            while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                end += 1;
            }
            if bytes.get(end) == Some(&b'm') {
                attrs.apply(&bytes[at + 2..end]);
            }
            at = (end + 1).min(bytes.len());
        }
        attrs
    }

    /// Apply the parameters of one SGR sequence.
    fn apply(&mut self, parameters: &[u8]) {
        for part in parameters.split(|b| *b == b';') {
            let number = std::str::from_utf8(part)
                .ok()
                .and_then(|s| s.trim().parse::<u8>().ok())
                .unwrap_or(0);
            match number {
                0 => *self = Attrs::default(),
                1 => self.bold = true,
                22 => self.bold = false,
                4 => self.underline = true,
                24 => self.underline = false,
                30..=37 => self.fg = Some(number),
                39 => self.fg = None,
                _ => {}
            }
        }
    }
}

impl Screen {
    /// Reconstruct the screen from the bytes of a capture.
    fn of(bytes: &[u8]) -> Screen {
        let mut screen = Screen { cells: Vec::new() };
        let mut cursor = 0usize;
        let mut attrs = Attrs::default();
        let mut at = 0usize;
        while at < bytes.len() {
            let byte = bytes[at];
            if byte == 0x1b {
                // An escape sequence. The one that matters is the CSI
                // sequence: `ESC [` and then parameters and a final byte.
                let Some(&b'[') = bytes.get(at + 1) else {
                    // Some other escape: skip the escape and the byte
                    // after it.
                    at += 2;
                    continue;
                };
                let mut end = at + 2;
                while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                    end += 1;
                }
                let final_byte = bytes.get(end).copied();
                let parameters = &bytes[at + 2..end.min(bytes.len())];
                match final_byte {
                    Some(b'm') => attrs.apply(parameters),
                    Some(b'C') => cursor += count(parameters, 1),
                    Some(b'D') => cursor = cursor.saturating_sub(count(parameters, 1)),
                    Some(b'K') | Some(b'J') => {
                        // Erase from the cursor to the end of the line.
                        screen.cells.truncate(cursor.min(screen.cells.len()));
                    }
                    _ => {}
                }
                at = (end + 1).min(bytes.len());
                continue;
            }
            if byte >= 0x20 {
                let cell = Cell { byte, attrs };
                if cursor < screen.cells.len() {
                    screen.cells[cursor] = cell;
                } else {
                    screen.cells.push(cell);
                }
                cursor += 1;
            }
            at += 1;
        }
        screen
    }

    /// The positions at which `needle` appears on the screen.
    fn occurrences(&self, needle: &[u8]) -> Vec<usize> {
        if needle.is_empty() || needle.len() > self.cells.len() {
            return Vec::new();
        }
        self.cells
            .windows(needle.len())
            .enumerate()
            .filter(|(_, window)| window.iter().map(|c| c.byte).eq(needle.iter().copied()))
            .map(|(at, _)| at)
            .collect()
    }

    /// The positions of the characters drawn with these attributes.
    fn positions_with(&self, attrs: Attrs) -> Vec<usize> {
        self.cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.attrs == attrs)
            .map(|(at, _)| at)
            .collect()
    }

    /// The attributes that were in force at a position of the screen.
    fn attrs_at(&self, position: usize) -> Option<Attrs> {
        self.cells.get(position).map(|cell| cell.attrs)
    }
}

/// The number a cursor-motion sequence moves by.
///
/// The sequence is `ESC [ <digits> <letter>`; a sequence with no digits
/// moves by one.
fn count(parameters: &[u8], default: usize) -> usize {
    let digits: Vec<u8> = parameters
        .iter()
        .copied()
        .take_while(|b| b.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return default;
    }
    std::str::from_utf8(&digits)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(default)
}

/// A scenario to run under the pseudo-terminal.
struct Scenario {
    /// The text typed into the line editor.
    ///
    /// A `String` and not a `&'static str`: a scenario may have to type
    /// a path that is only known when the test runs.
    typed: String,
    /// The bytes the harness waits for after typing.
    wait_for: &'static [u8],
    /// Whether the shell under the pty loads the module.
    load_module: bool,
    /// How many times the marker is expected to appear.
    count: usize,
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
    let dir = std::env::temp_dir().join(format!("zsh-pulse-pty-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the temporary directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    dir
}

/// Write the configuration the shell under the pty starts with.
///
/// `ZDOTDIR` points at this directory, so the interactive shell reads
/// `.zshrc` from here and from nowhere else: the user's own configuration
/// cannot influence the result.
///
/// `extra_rc` runs before the adapter, `after_rc` after it. Completion
/// fixtures need the latter: they register with `compdef`, which only
/// exists after `compinit` and the plugin have installed the completion
/// system.
fn write_child_rc_full(dir: &Path, load_module: bool, extra_rc: &str, after_rc: &str) {
    let mut rc = String::new();
    rc.push_str(&format!("PS1='{PROMPT}'\n"));
    rc.push_str("RPROMPT=\n");
    rc.push_str("PROMPT_EOL_MARK=''\n");
    rc.push_str("HISTFILE=${ZDOTDIR}/.zsh_history\n");
    rc.push_str("HISTSIZE=1000\n");
    rc.push_str("SAVEHIST=1000\n");
    rc.push_str("setopt EXTENDED_HISTORY 2>/dev/null\n");
    rc.push_str("export ZSH_PULSE_USE_FZF=0\n");
    rc.push_str(extra_rc);
    if load_module {
        // `-u` avoids an interactive "insecure directories" prompt on CI
        // runners where the temporary ZDOTDIR permissions trip compaudit.
        rc.push_str("autoload -Uz compinit && compinit -u -D 2>/dev/null\n");
        // The adapter looks for the module here, exactly as a user who
        // ran `bench/install.sh` would have it.
        rc.push_str(&format!(
            "export ZSH_PULSE_MODULE_PATH='{}'\n",
            install_module().display()
        ));
        rc.push_str(&format!("source '{}'\n", adapter_path().display()));
        rc.push_str(after_rc);
    }
    // Proof that the module really loaded. Without it, "nothing was
    // highlighted" would prove nothing at all.
    rc.push_str(&format!(
        "if (( ${{+builtins[zsh-pulse-highlight]}} )); then print -r -- {MODULE_LOADED}; fi\n"
    ));
    fs::write(dir.join(".zshrc"), rc).expect("write the child's .zshrc");
}

#[allow(dead_code)]
fn capture_with_history(
    scenario: &Scenario,
    term: &str,
    label: &str,
    history: &[u8],
) -> Result<Capture, String> {
    capture_full(scenario, term, label, history, "", "", None)
}

#[allow(dead_code)]
fn capture_with_history_extra(
    scenario: &Scenario,
    term: &str,
    label: &str,
    history: &[u8],
    extra_rc: &str,
) -> Result<Capture, String> {
    capture_full(scenario, term, label, history, extra_rc, "", None)
}

/// Run one scenario under a pseudo-terminal.
///
/// Returns `Err(reason)` when this environment cannot provide a
/// pseudo-terminal. That is a documented skip, not a failure: the
/// pseudo-terminal is a test dependency, and the plugin has no say in
/// whether the machine has one.
///
/// The reason is *returned* and not printed here: the caller prints it,
/// so that the skip markers come from the test itself and a caller
/// cannot forget to print them.
fn capture(scenario: &Scenario, term: &str, label: &str) -> Result<Capture, String> {
    capture_full(scenario, term, label, b"", "", "", None)
}

/// Run one scenario with paced key input.
///
/// `keys` are sent one at a time, with the harness draining the terminal
/// between them. That is required for typeahead: the plugin skips the
/// preview while `KEYS_QUEUED_COUNT` is non-zero, so typing a whole line
/// at once would suppress it. The returned [`Capture`] carries one
/// snapshot offset per key.
fn capture_keys(
    scenario: &Scenario,
    term: &str,
    label: &str,
    keys: &[&[u8]],
    after_rc: &str,
) -> Result<Capture, String> {
    let mut key_file = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            key_file.push(0x1f);
        }
        key_file.extend_from_slice(key);
    }
    // A sentinel byte (ASCII record separator) keeps command substitution
    // from stripping a final Enter key's newline. The harness removes it
    // before splitting on the unit separators.
    key_file.push(0x1e);
    capture_full(scenario, term, label, b"", "", after_rc, Some(&key_file))
}

#[allow(clippy::too_many_arguments)]
fn capture_full(
    scenario: &Scenario,
    term: &str,
    label: &str,
    history: &[u8],
    before_rc: &str,
    after_rc: &str,
    keys: Option<&[u8]>,
) -> Result<Capture, String> {
    let dir = temp_dir(label);

    let harness = dir.join("harness.zsh");
    fs::write(&harness, HARNESS).expect("write the pty harness");
    let ready = dir.join("ready.bin");
    fs::write(&ready, PROMPT.as_bytes()).expect("write the ready marker");
    let done = dir.join("done.bin");
    fs::write(&done, scenario.wait_for).expect("write the wait marker");
    let out = dir.join("capture.bin");
    let keys_file = dir.join("keys.bin");
    let snapshots_file = dir.join("snapshots.txt");

    let rc_dir = dir.join("rc");
    fs::create_dir_all(&rc_dir).expect("create the rc directory");
    fs::write(rc_dir.join(".zsh_history"), history).expect("write history");
    write_child_rc_full(&rc_dir, scenario.load_module, before_rc, after_rc);

    let mut command = Command::new("zsh");
    command
        .arg("-f")
        .arg(&harness)
        .env("SP_READY_FILE", &ready)
        .env("SP_DONE_FILE", &done)
        .env("SP_DONE_COUNT", scenario.count.to_string())
        .env("SP_OUT", &out)
        .env("SP_TIMEOUT", "10")
        .env("ZDOTDIR", &rc_dir)
        .env("TERM", term)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(keys) = keys {
        fs::write(&keys_file, keys).expect("write the paced keys");
        command
            .env("SP_KEYS_FILE", &keys_file)
            .env("SP_SNAPSHOTS", &snapshots_file);
    } else {
        command.env("SP_TYPE", &scenario.typed);
    }

    let child = command.spawn().expect("spawn the pty harness");
    let mut guard = ChildGuard(child);

    let status = wait_with_timeout(&mut guard.0, Duration::from_secs(30))
        .expect("wait for the pty harness")
        .unwrap_or_else(|| {
            panic!("the pty harness did not finish within 30 s: the pseudo-terminal hung")
        });

    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = guard.0.stdout.take() {
        pipe.read_to_string(&mut stdout)
            .expect("read the harness output");
    }
    if let Some(mut pipe) = guard.0.stderr.take() {
        pipe.read_to_string(&mut stderr)
            .expect("read the harness diagnostics");
    }

    // Exit code 3 is the harness's way of saying "this machine cannot run
    // the pseudo-terminal". It is a skip, and the reason is the harness's
    // own message.
    if status.code() == Some(3) {
        return Err(stderr.trim().to_string());
    }

    assert!(
        status.success(),
        "the pty harness failed ({status})\nstdout: {stdout}\nstderr: {stderr}"
    );

    let bytes = fs::read(&out).expect("read the captured bytes");
    let snapshots = if keys.is_some() {
        fs::read_to_string(&snapshots_file)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse::<usize>().ok())
            .collect()
    } else {
        Vec::new()
    };
    // The scratch directory is not needed any more; the bytes it held are
    // in memory now.
    let _ = fs::remove_dir_all(&dir);
    Ok(Capture {
        bytes,
        snapshots,
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

/// The `TERM` to run the pseudo-terminal under, and the bytes it produces.
///
/// The bytes zsh writes for an attribute come from the terminal's
/// capabilities, so the exact sequence depends on `TERM`. The test pins
/// `TERM` to a terminal whose capabilities are the standard xterm ones
/// and checks that with zsh itself before using it: `%B` is the bold
/// sequence, `%U` the underline one and `%F{cyan}` the foreground-colour
/// one. Asserting on bytes this test did not measure would be a guess,
/// and a terminal that answers differently would make the guess wrong.
///
/// Every sequence an assertion in this file uses is checked, not only the
/// two the first version of this test needed: a terminal that answers
/// `cyan` correctly but `magenta` differently would make the later
/// assertions wrong.
fn colour_terminal() -> Option<Terminal> {
    const CANDIDATES: &[&str] = &["xterm-256color", "xterm", "screen", "linux", "xterm-kitty"];
    CANDIDATES.iter().copied().find_map(|name| {
        let palette = Palette::measure(name);
        palette
            .is_standard_xterm()
            .then_some(Terminal { name, palette })
    })
}

#[test]
#[cfg(feature = "highlight")]
fn the_typed_command_is_highlighted_on_the_terminal() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip(
            "no terminal type with the standard xterm colour capabilities \
             was found, so the bytes zsh writes for `fg=green` cannot be known \
             in advance and asserting on them would be a guess.",
        );
        return;
    };
    let capture = match capture(
        &Scenario {
            typed: "echo SPARGXYZ".to_string(),
            wait_for: ARGUMENT.as_bytes(),
            load_module: true,
            count: 1,
        },
        term.name,
        "highlighted",
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    // The module really loaded: without this the whole scenario would be
    // testing a shell that never had the plugin in it.
    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load in the shell under the pty, so nothing could \
         have been highlighted.\n{}",
        capture.dump()
    );

    // The command word is `fg=green`: the shell knows `echo`, so the
    // plugin paints it green.
    assert!(
        capture.contains(&term.palette.green),
        "the green region of the command word never reached the terminal.\n\
         expected the bytes {:?} somewhere in the terminal output.\n\
         harness: {}\ncaptured {} bytes:\n{}",
        term.palette.green,
        capture.status,
        capture.bytes.len(),
        capture.dump()
    );

    // The region has to be closed again, or the rest of the line would be
    // drawn in the command's colour.
    assert!(
        capture.contains(&term.palette.default_foreground),
        "the highlighted region was never closed: expected the bytes \
         {:?}.\n{}",
        term.palette.default_foreground,
        capture.dump()
    );

    // The argument was typed and drawn.
    let drawn = capture.screen_occurrences(ARGUMENT.as_bytes());
    assert!(
        !drawn.is_empty(),
        "the argument {ARGUMENT} was never drawn, so the line was not edited \
         at all.\n{}",
        capture.dump()
    );

    // The highlight stops at the end of the command word: an ordinary
    // argument is not styled, so the bytes in front of the argument cannot
    // be the green sequence.
    for at in drawn {
        let before = &capture.bytes[at.saturating_sub(term.palette.green.len())..at];
        assert_ne!(
            before,
            term.palette.green.as_slice(),
            "the argument {ARGUMENT} was drawn inside the highlighted region: \
             an ordinary argument must not be styled.\n{}",
            capture.dump()
        );
    }
}

#[test]
#[cfg(feature = "highlight")]
fn an_empty_line_is_not_highlighted() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip(
            "no terminal type with the standard xterm \
             capabilities was found, so there is nothing to compare against.",
        );
        return;
    };
    let capture = match capture(
        &Scenario {
            typed: String::new(),
            wait_for: PROMPT.as_bytes(),
            load_module: true,
            count: 1,
        },
        term.name,
        "empty",
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    // The module really loaded, so the absence of colour below is the
    // highlighter's decision and not a broken setup.
    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load in the shell under the pty.\n{}",
        capture.dump()
    );
    // Not one of the sequences the plugin can write may be there: the
    // plugin publishes six colours and two attributes, and an empty line
    // has none of them. Checking a single colour would let the others
    // through.
    for sequence in term.palette.every_sequence() {
        assert!(
            !capture.contains(sequence),
            "an empty line has no styled region, so nothing may be drawn in \
             colour, but the bytes {sequence:?} are in the terminal output.\n\
             captured {} bytes:\n{}",
            capture.bytes.len(),
            capture.dump()
        );
    }
}

/// The highlighting has to survive a second command line.
///
/// A plugin that works on the first line and then goes quiet looks exactly
/// like one that works, until you press Enter. Two things can break it: the
/// hook being replaced by another plugin, and the module believing it still
/// holds the previous line's regions after zsh has emptied them.
#[test]
#[cfg(feature = "highlight")]
fn the_highlighting_survives_a_second_command_line() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip(
            "no terminal type with the standard xterm colour capabilities \
             was found, so the bytes zsh writes for `fg=green` cannot be known \
             in advance and asserting on them would be a guess.",
        );
        return;
    };
    let capture = match capture(
        &Scenario {
            // Two *different* markers, one per line. With the same marker
            // on both lines the wait would be satisfied by the first line
            // alone: `SPARGXYZ` appears once as the drawn argument and
            // once as the output of the `echo`, so the second line would
            // never have to be drawn at all.
            typed: "echo SPARGXYZ\necho SPARGZULU".to_string(),
            wait_for: SECOND_ARGUMENT.as_bytes(),
            load_module: true,
            count: 1,
        },
        term.name,
        "two-lines",
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load in the shell under the pty.\n{}",
        capture.dump()
    );

    // Both lines were drawn.
    let first = capture.screen_occurrences(ARGUMENT.as_bytes());
    let second = capture.screen_occurrences(SECOND_ARGUMENT.as_bytes());
    assert!(
        !first.is_empty(),
        "the argument {ARGUMENT} of the first line was never drawn, so the \
         first line was not edited at all.\n{}",
        capture.dump()
    );
    assert!(
        !second.is_empty(),
        "the argument {SECOND_ARGUMENT} of the second line was never drawn, \
         so the second line was not edited at all.\n{}",
        capture.dump()
    );

    // The command word of *each* line must have been highlighted: the
    // first one proves the hook works, the second proves it still works
    // after the line was submitted and zsh emptied the display array.
    //
    // The green of the first line is drawn before that line's argument;
    // the green of the second line is drawn after it — between the two
    // arguments there is only the output of the first `echo` and the
    // prompt of the second line, and neither of them is coloured.
    let green = capture.screen_positions_with(Attrs::of(&term.palette.green));
    assert!(
        !green.is_empty(),
        "the command word was never highlighted in green.\n{}",
        capture.dump()
    );
    let first_at = first[0];
    let second_at = second[0];
    assert!(
        green.iter().any(|at| *at < first_at),
        "the command word of the *first* line was never highlighted in green.\n\
         harness: {}\ncaptured {} bytes:\n{}",
        capture.status,
        capture.bytes.len(),
        capture.dump()
    );
    assert!(
        green.iter().any(|at| *at > first_at && *at < second_at),
        "the command word of the *second* line was never highlighted in \
         green: every green is outside the second line.\n\
         harness: {}\ncaptured {} bytes:\n{}",
        capture.status,
        capture.bytes.len(),
        capture.dump()
    );
}

/// A shell completion via Tab must never kill syntax highlighting on subsequent lines or commands.
/// Previously, Zsh's `_main_complete` set global `MENUSELECT=0`, causing `zsh-pulse-highlight-redraw`
/// to permanently bail out on `-n $MENUSELECT`.
#[test]
#[cfg(feature = "highlight")]
fn the_highlighting_survives_tab_completion() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };
    let capture = match capture(
        &Scenario {
            // Type command, trigger tab completion, accept/submit with newlines, then type a command on next line.
            typed: "echo SPARGXYZ\t\r\recho SPARGZULU".to_string(),
            wait_for: SECOND_ARGUMENT.as_bytes(),
            load_module: true,
            count: 1,
        },
        term.name,
        "tab-highlight",
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load in the shell under the pty.\n{}",
        capture.dump()
    );

    let second = capture.screen_occurrences(SECOND_ARGUMENT.as_bytes());
    assert!(
        !second.is_empty(),
        "the argument {SECOND_ARGUMENT} of the second line was never drawn.\n{}",
        capture.dump()
    );

    let green = capture.screen_positions_with(Attrs::of(&term.palette.green));
    let second_at = second[0];
    assert!(
        green.iter().any(|at| *at < second_at),
        "highlighting was killed after tab completion: no green command word drawn on the second line.\n{}",
        capture.dump()
    );
}

#[test]
fn without_the_module_nothing_is_highlighted() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip(
            "no terminal type with the standard xterm \
             capabilities was found, so there is nothing to compare against.",
        );
        return;
    };
    let capture = match capture(
        &Scenario {
            typed: "echo SPARGXYZ".to_string(),
            wait_for: ARGUMENT.as_bytes(),
            load_module: false,
            count: 1,
        },
        term.name,
        "no-module",
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    // The control: zsh itself must not colour the line, or the assertion
    // in the first test would prove nothing about zsh-pulse.
    assert!(
        !capture.contains(MODULE_LOADED.as_bytes()),
        "the module was loaded although this scenario asks for a shell \
         without it.\n{}",
        capture.dump()
    );
    assert!(
        !capture.screen_occurrences(ARGUMENT.as_bytes()).is_empty(),
        "the line was never drawn, so there is nothing to compare against.\n{}",
        capture.dump()
    );
    // Not one of the sequences the plugin can write may be there. zsh
    // itself must not colour the command line; if it does, the other
    // tests in this file cannot tell zsh-pulse's highlighting apart
    // from zsh's own.
    for sequence in term.palette.every_sequence() {
        assert!(
            !capture.contains(sequence),
            "zsh itself must not colour the command line; if it does, the \
             other tests in this file cannot tell zsh-pulse's highlighting \
             apart from zsh's own, but the bytes {sequence:?} are in the \
             terminal output.\ncaptured {} bytes:\n{}",
            capture.bytes.len(),
            capture.dump()
        );
    }
}

/// A command the shell knows is green.
///
/// `echo` is a builtin, so the shell knows it and the plugin paints the
/// command word green.
///
/// The prefixes of the word are a different matter: while the word is
/// being typed the shell is asked about `e`, `ec` and `ech`, none of
/// which it knows, so those are red. The *last* red therefore belongs to
/// a prefix and has to come before the green of the finished word — a
/// plugin that painted `echo` red would put a red after the green.
#[test]
#[cfg(feature = "highlight")]
fn a_known_command_is_green_on_the_terminal() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip(
            "no terminal type with the standard xterm colour capabilities \
             was found, so the bytes zsh writes for `fg=green` cannot be known \
             in advance and asserting on them would be a guess.",
        );
        return;
    };
    let capture = match capture(
        &Scenario {
            typed: "echo SPARGXYZ".to_string(),
            wait_for: ARGUMENT.as_bytes(),
            load_module: true,
            count: 1,
        },
        term.name,
        "known-command",
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    // The module really loaded: without it the whole scenario would be
    // testing a shell that never had the plugin in it.
    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load in the shell under the pty, so nothing could \
         have been highlighted.\n{}",
        capture.dump()
    );

    // The green reached the terminal.
    let green = capture.screen_positions_with(Attrs::of(&term.palette.green));
    assert!(
        !green.is_empty(),
        "the command word was not painted green: the bytes {:?} never \
         reached the terminal.\n\
         harness: {}\ncaptured {} bytes:\n{}",
        term.palette.green,
        capture.status,
        capture.bytes.len(),
        capture.dump()
    );

    // The *finished* word is the green one. Every red the plugin painted
    // belongs to a prefix the shell did not know yet, and those are drawn
    // first.
    let red = capture.screen_positions_with(Attrs::of(&term.palette.red_bold));
    let last_red = red.last().copied();
    let last_green = *green.last().expect("the green was checked above");
    assert!(
        last_red.is_none_or(|last| last < last_green),
        "the command word was painted red although the shell knows `echo`: \
         the last red is at offset {last_red:?} and the last green at offset \
         {last_green}.\n{}",
        capture.dump()
    );

    // The green region was closed before the argument was drawn: the
    // attributes in force at the argument are the default ones.
    let drawn = capture.screen_occurrences(ARGUMENT.as_bytes());
    assert!(
        !drawn.is_empty(),
        "the argument {ARGUMENT} was never drawn, so the line was not edited \
         at all.\n{}",
        capture.dump()
    );
    for at in drawn {
        assert_eq!(
            capture.attrs_at(at),
            Some(Attrs::of(&term.palette.default_foreground)),
            "the command word was still in green when the argument was \
             drawn, so the region was never closed.\n{}",
            capture.dump()
        );
    }
}

/// A command the shell does not know is red and bold.
///
/// The word `nosuchcommandxyz` is not a builtin, not a command on `$PATH`
/// and not an alias, so the plugin paints it red and bold. Its prefixes
/// are unknown too — `n`, `no`, `nos` — so the whole word is red and
/// *no* green may appear: a green would mean the plugin thought the
/// shell knew the word.
#[test]
#[cfg(feature = "highlight")]
fn an_unknown_command_is_red_and_bold_on_the_terminal() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip(
            "no terminal type with the standard xterm colour capabilities \
             was found, so the bytes zsh writes for `fg=red,bold` cannot be \
             known in advance and asserting on them would be a guess.",
        );
        return;
    };
    let capture = match capture(
        &Scenario {
            typed: "nosuchcommandxyz SPARGXYZ".to_string(),
            wait_for: ARGUMENT.as_bytes(),
            load_module: true,
            count: 1,
        },
        term.name,
        "unknown-command",
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    // The module really loaded: without it the whole scenario would be
    // testing a shell that never had the plugin in it.
    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load in the shell under the pty, so nothing could \
         have been highlighted.\n{}",
        capture.dump()
    );

    // The red and bold reached the terminal.
    assert!(
        capture.contains(&term.palette.red_bold),
        "the red+bold region of the unknown command never reached the \
         terminal.\nexpected the bytes {:?} somewhere in the terminal \
         output.\nharness: {}\ncaptured {} bytes:\n{}",
        term.palette.red_bold,
        capture.status,
        capture.bytes.len(),
        capture.dump()
    );

    // No green at all: the shell does not know this word, so nothing may
    // be painted green. This is the check that keeps the test honest — a
    // plugin that painted every word green would fail here.
    assert!(
        !capture.contains(&term.palette.green),
        "the unknown command was painted green: the bytes {:?} are in the \
         terminal output, but the shell does not know `nosuchcommandxyz`.\n\
         harness: {}\ncaptured {} bytes:\n{}",
        term.palette.green,
        capture.status,
        capture.bytes.len(),
        capture.dump()
    );

    // The red region was closed before the argument was drawn: the
    // attributes in force at the argument are the default ones.
    let drawn = capture.screen_occurrences(ARGUMENT.as_bytes());
    assert!(
        !drawn.is_empty(),
        "the argument {ARGUMENT} was never drawn, so the line was not edited \
         at all.\n{}",
        capture.dump()
    );
    for at in drawn {
        assert_eq!(
            capture.attrs_at(at),
            Some(Attrs::of(&term.palette.default_foreground)),
            "the command word was still in red when the argument was drawn, \
             so the region was never closed.\n{}",
            capture.dump()
        );
    }
}

/// A command-line option is cyan.
///
/// `ls` is a command the shell knows, so the command word is green; the
/// option `--color` that follows it is cyan. The option is further right
/// on the line than the command word, so its cyan has to come after the
/// green — and its region has to be closed before the argument is drawn.
#[test]
#[cfg(feature = "highlight")]
fn an_option_is_highlighted_on_the_terminal() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip(
            "no terminal type with the standard xterm colour capabilities \
             was found, so the bytes zsh writes for `fg=cyan` cannot be known \
             in advance and asserting on them would be a guess.",
        );
        return;
    };
    let capture = match capture(
        &Scenario {
            typed: "ls --color SPARGXYZ".to_string(),
            wait_for: ARGUMENT.as_bytes(),
            load_module: true,
            count: 1,
        },
        term.name,
        "option",
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    // The module really loaded: without it the whole scenario would be
    // testing a shell that never had the plugin in it.
    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load in the shell under the pty, so nothing could \
         have been highlighted.\n{}",
        capture.dump()
    );

    // The cyan reached the terminal.
    let cyan = capture.screen_positions_with(Attrs::of(&term.palette.cyan));
    assert!(
        !cyan.is_empty(),
        "the option was not painted cyan: the bytes {:?} never reached the \
         terminal.\nharness: {}\ncaptured {} bytes:\n{}",
        term.palette.cyan,
        capture.status,
        capture.bytes.len(),
        capture.dump()
    );

    // The command word comes first on the line, so the option's cyan has
    // to come after the command word's green.
    let green = capture.screen_positions_with(Attrs::of(&term.palette.green));
    assert!(
        !green.is_empty(),
        "the command word `ls` was not painted green, so the order of the \
         two regions cannot be checked.\n{}",
        capture.dump()
    );
    assert!(
        green[0] < cyan[0],
        "the option was painted before the command word: the first green is \
         at offset {} and the first cyan at offset {}.\n{}",
        green[0],
        cyan[0],
        capture.dump()
    );

    // The option's region was closed before the argument was drawn: the
    // attributes in force at the argument are the default ones.
    let drawn = capture.screen_occurrences(ARGUMENT.as_bytes());
    assert!(
        !drawn.is_empty(),
        "the argument {ARGUMENT} was never drawn, so the line was not edited \
         at all.\n{}",
        capture.dump()
    );
    for at in drawn {
        assert_eq!(
            capture.attrs_at(at),
            Some(Attrs::of(&term.palette.default_foreground)),
            "the option was still in cyan when the argument was drawn, so \
             the region was never closed.\n{}",
            capture.dump()
        );
    }
}

/// A word naming an existing directory is magenta and underlined.
///
/// The plugin looks the word up in the filesystem: a word that names an
/// existing directory is magenta *and* underlined, so that it can be told
/// apart from a plain file, which is magenta only.
///
/// The directory used here is `std::env::temp_dir()` — the same place the
/// harness puts its own scratch directory, so it certainly exists.
#[test]
#[cfg(feature = "highlight")]
fn an_existing_directory_is_magenta_and_underlined_on_the_terminal() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip(
            "no terminal type with the standard xterm colour capabilities \
             was found, so the bytes zsh writes for `fg=magenta,underline` \
             cannot be known in advance and asserting on them would be a \
             guess.",
        );
        return;
    };
    let directory = std::env::temp_dir();
    // A path with a space in it cannot be typed as one word, so the
    // scenario would not be the one it claims to be.
    if directory.display().to_string().contains(' ') {
        skip(
            "the temporary directory name contains a space, so it cannot be \
             typed as a single word and the scenario would not test what it \
             claims to test.",
        );
        return;
    }
    let capture = match capture(
        &Scenario {
            typed: format!("ls {} SPARGXYZ", directory.display()),
            wait_for: ARGUMENT.as_bytes(),
            load_module: true,
            count: 1,
        },
        term.name,
        "directory",
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    // The module really loaded: without it the whole scenario would be
    // testing a shell that never had the plugin in it.
    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load in the shell under the pty, so nothing could \
         have been highlighted.\n{}",
        capture.dump()
    );

    // The magenta and the underline reached the terminal, in the order
    // zsh writes them: the attribute first, the colour second.
    assert!(
        capture.contains(&term.palette.magenta_underline),
        "the magenta+underline region of the directory never reached the \
         terminal.\nexpected the bytes {:?} somewhere in the terminal \
         output.\nharness: {}\ncaptured {} bytes:\n{}",
        term.palette.magenta_underline,
        capture.status,
        capture.bytes.len(),
        capture.dump()
    );
    assert!(
        capture.contains(&term.palette.magenta),
        "the magenta colour of the directory never reached the terminal.\n{}",
        capture.dump()
    );
    assert!(
        capture.contains(&term.palette.underline),
        "the underline of the directory never reached the terminal.\n{}",
        capture.dump()
    );

    // The argument was typed and drawn, outside the highlighted region.
    let drawn = capture.screen_occurrences(ARGUMENT.as_bytes());
    assert!(
        !drawn.is_empty(),
        "the argument {ARGUMENT} was never drawn, so the line was not edited \
         at all.\n{}",
        capture.dump()
    );
    for at in drawn {
        assert_eq!(
            capture.attrs_at(at),
            Some(Attrs::of(&term.palette.default_foreground)),
            "the directory was still underlined when the argument was drawn, \
             so the region was never closed.\n{}",
            capture.dump()
        );
    }
}

#[test]
#[cfg(feature = "highlight")]
fn typing_hello_is_not_hijacked_or_auto_completed() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("terminal not standard xterm");
        return;
    };

    let capture = match capture(
        &Scenario {
            typed: format!("hello {ARGUMENT}"),
            wait_for: ARGUMENT.as_bytes(),
            load_module: true,
            count: 1,
        },
        term.name,
        "hello_clean",
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load"
    );

    // The argument was typed and drawn without being blocked or hijacked.
    let drawn = capture.screen_occurrences(ARGUMENT.as_bytes());
    assert!(
        !drawn.is_empty(),
        "the line was not drawn, input was hijacked.\n{}",
        capture.dump()
    );

    // Must NOT contain auto-correction or compsys warning corruption.
    assert!(
        !capture.contains(b"delpart"),
        "found 'delpart' in capture — auto-correction hijacked input!\n{}",
        capture.dump()
    );
    assert!(
        !capture.contains(b"No matches for"),
        "found compsys error 'No matches for' on screen while typing!\n{}",
        capture.dump()
    );

    // "hello" is an unknown command, so it should be red+bold.
    assert!(
        capture.contains(&term.palette.red_bold),
        "hello should be highlighted in red+bold as an unknown command.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(any(feature = "history", feature = "history-navigation"))]
fn history_search_does_not_escape_spaces_in_command() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let history = b": 1700000000:0;git diff\n";
    let capture = match capture_with_history(
        &Scenario {
            typed: "\x12\n".to_string(), // Ctrl-R, Enter to accept
            wait_for: b"diff",
            load_module: true,
            count: 1,
        },
        term.name,
        "hist_no_escape",
        history,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load"
    );

    // Must NOT contain backslash escape "git\\ diff".
    assert!(
        !capture.contains(b"git\\ diff"),
        "found 'git\\ diff' with backslash space in capture — history completed with escaped spaces!\n{}",
        capture.dump()
    );

    // Must contain plain "git diff" on screen.
    let occurrences = capture.screen_occurrences(b"git diff");
    assert!(
        !occurrences.is_empty(),
        "did not find plain 'git diff' on screen.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(feature = "autocomplete")]
fn menu_select_down_arrow_accept_line_does_not_segfault() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let history = b": 1700000000:0;echo test_clean_exec\n";
    let capture = match capture_with_history(
        &Scenario {
            typed: "echo\x1b[B\r".to_string(), // type echo, Down-Arrow into menu-select, Enter to accept
            wait_for: b"test_clean_exec",
            load_module: true,
            count: 1,
        },
        term.name,
        "menu_no_segfault",
        history,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load"
    );

    assert!(
        capture.contains(b"test_clean_exec"),
        "command was not executed cleanly after menu-select.\n{}",
        capture.dump()
    );
}

/// A deterministic completion fixture for the typeahead tests.
///
/// Thirty candidates with a shared prefix make both halves of the contract
/// visible: the preview must stop at the line cap and leave `(MORE)`, and
/// Tab must show the rest of the same native set. `sp-test` prints its
/// arguments, so a test can tell whether a keystroke inserted a candidate
/// into the buffer.
///
/// It is installed *after* the adapter, because `compdef` only exists
/// after `compinit`.
const TYPEAHEAD_RC: &str = r#"
_sp_test_candidates() {
  local -a values
  integer i
  for i in {1..30}; do
    values+=("SPCANDA${(l:2::0:)i}")
  done
  _describe 'sp candidates' values
}
sp-test() { print -r -- "ARGS=$*" }
compdef _sp_test_candidates sp-test
"#;

/// The keys that type `sp-test ` one character at a time.
fn typeahead_keys() -> Vec<&'static [u8]> {
    b"sp-test "
        .iter()
        .map(std::slice::from_ref)
        .collect::<Vec<_>>()
}

/// The number of times `needle` occurs in one paced key's output.
fn segment_count(capture: &Capture, index: usize, needle: &[u8]) -> usize {
    capture
        .segment(index)
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

#[test]
#[cfg(feature = "autocomplete")]
fn typeahead_preview_is_capped_without_tab() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let keys = typeahead_keys();
    let last = keys.len() - 1;
    let capture = match capture_keys(
        &Scenario {
            typed: String::new(),
            wait_for: b"SPCANDA01",
            load_module: true,
            count: 1,
        },
        term.name,
        "typeahead_preview_cap",
        &keys,
        TYPEAHEAD_RC,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load"
    );
    assert!(
        capture.segment_contains(last, b"SPCANDA01"),
        "the preview did not list the first candidate without Tab.\n{}",
        capture.dump()
    );
    assert!(
        capture.segment_contains(last, b"(MORE)"),
        "the preview was not capped, so it has no (MORE) marker.\n{}",
        capture.dump()
    );
    assert!(
        !capture.segment_contains(last, b"SPCANDA30"),
        "the preview must not list candidates past the line cap.\n{}",
        capture.dump()
    );
    let previewed = segment_count(&capture, last, b"SPCANDA");
    assert!(
        previewed > 0 && previewed <= 16,
        "the preview drew {previewed} candidate lines, not a capped list.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(feature = "autocomplete")]
fn first_tab_shows_the_full_native_list() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let mut keys = typeahead_keys();
    let preview_index = keys.len() - 1;
    keys.push(b"\t");
    let tab_index = keys.len() - 1;

    let capture = match capture_keys(
        &Scenario {
            typed: String::new(),
            wait_for: b"SPCANDA30",
            load_module: true,
            count: 1,
        },
        term.name,
        "typeahead_tab_full_list",
        &keys,
        TYPEAHEAD_RC,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        !capture.segment_contains(preview_index, b"SPCANDA30"),
        "the capped preview should not contain the last candidate.\n{}",
        capture.dump()
    );
    assert!(
        capture.segment_contains(tab_index, b"SPCANDA30"),
        "the first Tab did not expand the full native list.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(feature = "autocomplete")]
fn the_preview_does_not_insert_a_candidate() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let mut keys = typeahead_keys();
    keys.push(b"\n");

    let capture = match capture_keys(
        &Scenario {
            typed: String::new(),
            wait_for: b"ARGS=",
            load_module: true,
            count: 1,
        },
        term.name,
        "typeahead_no_insert",
        &keys,
        TYPEAHEAD_RC,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(b"ARGS=\r\n"),
        "accepting the line should run the command with no arguments.\n{}",
        capture.dump()
    );
    assert!(
        !capture.contains(b"ARGS=SPCANDA"),
        "a preview candidate was inserted into the buffer.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(feature = "autocomplete")]
fn down_arrow_enters_menu_selection_from_the_preview() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let mut keys = typeahead_keys();
    keys.push(b"\x1b[B");
    let down_index = keys.len() - 1;
    keys.push(b"\n");

    let capture = match capture_keys(
        &Scenario {
            typed: String::new(),
            wait_for: b"ARGS=",
            load_module: true,
            count: 1,
        },
        term.name,
        "typeahead_down_select",
        &keys,
        TYPEAHEAD_RC,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.segment_contains(down_index, b"\x1b[7mSPCANDA01"),
        "Down-Arrow did not enter menu selection with the first candidate highlighted.\n{}",
        capture.dump()
    );
    assert!(
        capture.contains(b"ARGS=SPCANDA01"),
        "Enter after Down-Arrow did not accept the selected candidate.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(any(feature = "history", feature = "history-navigation"))]
fn history_search_single_down_arrow_selects_next_candidate() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let history = b": 1700000000:0;echo first_candidate\n: 1700000001:0;echo second_candidate\n: 1700000002:0;echo third_candidate\n";
    let capture = match capture_with_history(
        &Scenario {
            // Ctrl-R (opens menu on third_candidate), Down Arrow ONCE (moves to second_candidate), Enter (accepts)
            typed: "\x12\x1b[B\r".to_string(),
            wait_for: PROMPT.as_bytes(),
            load_module: true,
            count: 2,
        },
        term.name,
        "single_key_nav",
        history,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load"
    );

    // Verify second_candidate was actually executed on Enter (not third_candidate)
    assert!(
        capture.contains(b"second_candidate\r\n"),
        "second candidate command was not executed on a single down-arrow press.\n{}",
        capture.dump()
    );
    assert!(
        !capture.contains(b"third_candidate\r\n"),
        "third candidate executed instead of second candidate.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(any(feature = "history", feature = "history-navigation"))]
fn history_search_multiple_down_arrows_cycle_candidates() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let history = b": 1700000000:0;echo first_candidate\n: 1700000001:0;echo second_candidate\n: 1700000002:0;echo third_candidate\n";
    let capture = match capture_with_history(
        &Scenario {
            // Ctrl-R (opens menu on third_candidate), Down Arrow TWICE (third -> second -> first), Enter (accepts)
            typed: "\x12\x1b[B\x1b[B\r".to_string(),
            wait_for: PROMPT.as_bytes(),
            load_module: true,
            count: 2,
        },
        term.name,
        "multiple_down_arrows",
        history,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load"
    );

    // Verify first_candidate was actually executed after two down arrows
    assert!(
        capture.contains(b"first_candidate\r\n"),
        "first candidate was not executed after two down-arrow presses.\n{}",
        capture.dump()
    );
    assert!(
        !capture.contains(b"third_candidate\r\n"),
        "third candidate executed instead of first candidate.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(any(feature = "history", feature = "history-navigation"))]
fn ctrl_r_preserves_fzf_history_widget_when_present() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let history = b": 1700000000:0;echo regular_history\n";
    let extra_rc = r#"
export ZSH_PULSE_USE_FZF=1
fzf-history-widget() {
  BUFFER="echo FZF_HISTORY_INVOKED"
  CURSOR=$#BUFFER
  zle accept-line
}
zle -N fzf-history-widget
"#;

    let capture = match capture_with_history_extra(
        &Scenario {
            // Ctrl-R should invoke fzf-history-widget without being clobbered by zsh-pulse
            typed: "\x12".to_string(),
            wait_for: PROMPT.as_bytes(),
            load_module: true,
            count: 2,
        },
        term.name,
        "fzf_preserve",
        history,
        extra_rc,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load"
    );

    assert!(
        capture.contains(b"FZF_HISTORY_INVOKED\r\n"),
        "fzf-history-widget was not preserved on Ctrl-R.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(any(feature = "history", feature = "history-navigation"))]
fn history_search_abort_cancels_cleanly() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let history = b": 1700000000:0;echo first_candidate\n: 1700000001:0;echo second_candidate\n: 1700000002:0;echo my_history_match\n";
    let capture = match capture_with_history(
        &Scenario {
            // Type matching prefix "echo my_", press Ctrl-R (opens menu on my_history_match),
            // cancel with Ctrl-C (\x03), press Enter (\r)
            typed: "echo my_\x12\x03\r".to_string(),
            wait_for: PROMPT.as_bytes(),
            load_module: true,
            count: 2,
        },
        term.name,
        "abort_clean",
        history,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load"
    );

    // Verify history candidate was NOT executed on Ctrl-C cancellation
    assert!(
        !capture.contains(b"my_history_match\r\n"),
        "history candidate executed despite Ctrl-C cancellation.\n{}",
        capture.dump()
    );
    assert!(
        !capture.contains(b"first_candidate\r\n"),
        "candidate executed despite Ctrl-C cancellation.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(any(feature = "history", feature = "history-navigation"))]
fn history_search_abort_on_empty_line_cleans_up() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let history = b": 1700000000:0;echo first_candidate\n: 1700000001:0;echo second_candidate\n: 1700000002:0;echo third_candidate\n";
    let capture = match capture_with_history(
        &Scenario {
            // Press Ctrl-R on empty buffer, cancel with Ctrl-G (\x07), press Enter (\r)
            typed: "\x12\x07\r".to_string(),
            wait_for: PROMPT.as_bytes(),
            load_module: true,
            count: 2,
        },
        term.name,
        "abort_empty",
        history,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load"
    );

    // Verify nothing executed when aborted from an empty buffer
    assert!(
        !capture.contains(b"third_candidate\r\n"),
        "third candidate executed after abort on empty line.\n{}",
        capture.dump()
    );
    assert!(
        !capture.contains(b"second_candidate\r\n"),
        "second candidate executed after abort on empty line.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(any(feature = "history", feature = "history-navigation"))]
fn ctrl_r_fzf_fallback_strips_history_numbers() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    // Create a mock fzf script that simulates fzf returning a formatted history line
    let mock_dir = temp_dir("mock_fzf");
    let mock_fzf = mock_dir.join("fzf");
    let mock_script = "#!/bin/sh\nprintf '  123* echo FZF_FALLBACK_STRIPPED_OK\\n'\n";
    fs::write(&mock_fzf, mock_script).expect("write mock fzf");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&mock_fzf).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&mock_fzf, perms).expect("chmod mock fzf");
    }

    let history = b": 1700000000:0;echo dummy\n";
    let extra_rc = format!(
        "export PATH=\"{}:$PATH\"\nexport ZSH_PULSE_USE_FZF=1\n",
        mock_dir.display()
    );

    let capture = match capture_with_history_extra(
        &Scenario {
            // Ctrl-R triggers _zsh_pulse_fzf_history_widget (mock fzf), then Enter accepts and executes
            typed: "\x12\r".to_string(),
            wait_for: PROMPT.as_bytes(),
            load_module: true,
            count: 3,
        },
        term.name,
        "fzf_strip",
        history,
        &extra_rc,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load"
    );

    assert!(
        capture.contains(b"FZF_FALLBACK_STRIPPED_OK\r\n"),
        "fzf fallback did not cleanly strip history line numbers or execute command.\n{}",
        capture.dump()
    );
    assert!(
        !capture.contains(b"123"),
        "raw history number 123 leaked into executed buffer.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(any(feature = "history", feature = "history-navigation"))]
fn history_navigation_up_and_down_across_multiline_entries() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let history = b": 1700000000:0;echo cmd_one\n: 1700000001:0;printf 'multiline_target\\n'\n: 1700000002:0;echo cmd_three\n";
    let capture = match capture_with_history(
        &Scenario {
            // Up (cmd_three), Up (multiline_target), Up (cmd_one), Down (multiline_target), Enter
            typed: "\x1b[A\x1b[A\x1b[A\x1b[B\r".to_string(),
            wait_for: PROMPT.as_bytes(),
            load_module: true,
            count: 2,
        },
        term.name,
        "hist_multiline_nav",
        history,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load"
    );

    assert!(
        capture.contains(b"multiline_target\r\n"),
        "multiline entry was not cleanly traversed and executed on Up/Down sequence.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(any(feature = "history", feature = "history-navigation"))]
fn history_prefix_search_up_down_restores_original_buffer() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let history =
        b": 1700000000:0;echo match_old\n: 1700000001:0;ls other\n: 1700000002:0;echo match_new\n";
    let capture = match capture_with_history(
        &Scenario {
            // Type "echo match_", Up (match_new), Up (match_old), Down (match_new), Down (echo match_), type "custom_tail", Enter
            typed: "echo match_\x1b[A\x1b[A\x1b[B\x1b[Bcustom_tail\r".to_string(),
            wait_for: PROMPT.as_bytes(),
            load_module: true,
            count: 2,
        },
        term.name,
        "hist_prefix_restore",
        history,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load"
    );

    assert!(
        capture.contains(b"match_custom_tail\r\n"),
        "original prefix was not restored when traversing down to start of search.\n{}",
        capture.dump()
    );
}

#[test]
#[cfg(any(feature = "history", feature = "history-navigation"))]
fn history_navigation_multiline_with_backslash_continuations() {
    require_zsh_pty!();
    let Some(term) = colour_terminal() else {
        skip("no terminal type with standard xterm colour capabilities was found");
        return;
    };

    let history = b": 1700000000:0;echo cmd_old\n: 1700000001:0;echo multiline_part1; \\\n  echo multiline_part2; \\\n  echo multiline_part3\n: 1700000002:0;echo cmd_new\n";
    let capture = match capture_with_history(
        &Scenario {
            // Up to cmd_new, Up to multiline command, Up to cmd_old, Down to multiline command, Enter
            typed: "\x1b[A\x1b[A\x1b[A\x1b[B\r".to_string(),
            wait_for: PROMPT.as_bytes(),
            load_module: true,
            count: 2,
        },
        term.name,
        "hist_backslash_multiline",
        history,
    ) {
        Ok(capture) => capture,
        Err(reason) => {
            skip(&reason);
            return;
        }
    };

    assert!(
        capture.contains(MODULE_LOADED.as_bytes()),
        "the module did not load"
    );

    assert!(
        capture.contains(b"multiline_part1\r\nmultiline_part2\r\nmultiline_part3\r\n"),
        "multiline command with continuations was not cleanly navigated and executed.\n{}",
        capture.dump()
    );
}

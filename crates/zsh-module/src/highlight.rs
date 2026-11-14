//! Syntax highlighting builtin: `zsh-pulse-highlight`.

use std::{
    os::unix::ffi::OsStrExt,
    sync::OnceLock,
    time::{Duration, Instant},
};

use {
    highlight::{ChangedRange, HighlightSpan, StyleId, char_offsets_into},
    lexer::{LexCache, Token, TokenKind},
    zsh_abi::{Api, CappedRead, guard, raw::CStr},
};

use crate::{debug, debug_enabled, engine, has_argument, region, report_degraded, zsh};

static ECHO: OnceLock<bool> = OnceLock::new();

fn echo_enabled() -> &'static bool {
    ECHO.get_or_init(|| std::env::var_os("ZSH_PULSE_ECHO").is_some())
}

// ---------------------------------------------------------------------------
// The builtin itself
// ---------------------------------------------------------------------------

/// The wall-clock budget for one keystroke.
///
/// The design target is ten microseconds per keystroke, so a millisecond is
/// a hundred times the budget: anything slower than that is not worth
/// showing, and pushing a late highlight makes the display flicker.
const BUDGET: Duration = Duration::from_millis(1);

/// The longest line that is worth highlighting.
///
/// The budget is a millisecond, and the measured cost of the highlighting
/// path is a few microseconds for a normal line, so a limit of a few
/// kilobytes leaves a very large margin. A command line longer than this is
/// almost always a paste, and highlighting it would cost more than the
/// result is worth.
const MAX_LINE: usize = 4 * 1024;

/// The state the highlighter keeps between keystrokes.
///
/// Everything in here is reused: an allocation would happen on the
/// interactive thread, between the user's keystroke and the redraw. The
/// buffers are swapped rather than copied, and the region entries are
/// built in one reused byte buffer, so a steady-state keystroke allocates
/// only the small vector of row pointers zsh is handed.
#[derive(Debug, Default)]
struct Highlighter {
    /// The incremental lexer.
    cache: LexCache,
    /// The spans of the line as it is now.
    ///
    /// These are the spans that are *published*, not the lexer's spans:
    /// two of the styles — whether the shell knows the command, and
    /// whether an argument names a file that exists — cannot be decided
    /// by the lexer.
    spans: Vec<HighlightSpan>,
    /// The spans that were last handed to zsh.
    ///
    /// The two are swapped rather than copied, so a keystroke does not
    /// allocate: the span buffer is reused whichever way the comparison goes.
    pushed_spans: Vec<HighlightSpan>,
    /// The `region_highlight` entries of the last line.
    entries: region::Entries,
    /// The byte-to-character map of the last line.
    ///
    /// Only built for lines that are not pure ASCII, where byte offsets and
    /// character offsets differ.
    offsets: Vec<u32>,
    /// Whether `region_highlight` exists.
    ///
    /// The ZLE special parameter is created for the duration of a widget
    /// (`zsh-src/Src/Zle/zle_params.c:141-188`) and does not exist outside
    /// one. Writing to a name that does not exist would *create* a global
    /// parameter of that name in the user's shell, so the module only ever
    /// writes to a parameter that is already there.
    ///
    /// The check is not free — zsh materialises the whole array to answer it
    /// — so the answer is cached and refreshed once per line, by `--reset`.
    region_highlight_exists: Option<bool>,
    /// Whether zsh currently holds the entries.
    ///
    /// The ZLE special parameters are created fresh for every widget call
    /// (`zsh-src/Src/Zle/zle_main.c:1533`), so the *parameter* is always
    /// empty when the builtin runs. The C array that actually drives the
    /// display (`zsh-src/Src/Zle/zle_refresh.c:227`) is not — but it is
    /// emptied at the end of every line
    /// (`zsh-src/Src/Zle/zle_main.c:1398`), so the flag is reset when a new
    /// line starts.
    pushed: bool,
    /// The answers to "does this path exist?".
    ///
    /// The same path is looked up on every keystroke, and the module is on
    /// the interactive thread, so the answers are remembered.
    paths: PathCache,
    /// The scratch the path lookups need.
    scratch: PathScratch,
    /// The cursor offset from the last redraw.
    last_cursor: Option<i64>,
    /// Whether the last published `region_highlight` contained non-ours entries.
    ///
    /// Used to take a cheaper clear path on bailout when no foreign regions
    /// need to be preserved.
    has_foreign_regions: bool,
}

impl Highlighter {
    /// Highlight `buffer` and, if anything changed, hand the result to zsh.
    ///
    /// `publish` says whether the caller wants the highlights written to
    /// `region_highlight` at all. Only the ZLE hook asks for it: writing to
    /// that parameter outside a widget would either do nothing (the
    /// parameter does not exist) or, worse, overwrite a global array of the
    /// same name that the user or another plugin owns.
    ///
    /// `too_long` says the line in zsh is longer than [`MAX_LINE`]. Such a
    /// line is not highlighted, and its bytes were not copied, so `buffer`
    /// is empty.
    ///
    /// `answers` is what the shell said about the command words of the
    /// line: the shell — not the module — knows whether a word is an
    /// alias, a function, a builtin, a command on `$PATH` or a reserved
    /// word.
    ///
    /// # Safety
    ///
    /// `api` must be the resolved zsh API and the call must happen inside
    /// zsh.
    fn update(
        &mut self,
        api: &Api,
        buffer: &[u8],
        cursor: Option<i64>,
        too_long: bool,
        publish: bool,
        answers: &CommandAnswers,
    ) {
        if too_long {
            // Too long to be worth highlighting. The highlights that are on
            // screen describe the *previous* line, so they must go: leaving
            // them would show the styling of a line that is no longer there.
            self.clear(api, publish);
            return;
        }

        let started = Instant::now();

        // Re-lex. A line the lexer cannot represent exactly (too deeply
        // nested, an impossible here-document) is left unstyled rather than
        // styled wrongly.
        let outcome = match self.cache.relex(buffer) {
            Ok(o) => o,
            Err(_) => {
                self.clear(api, publish);
                return;
            }
        };
        if outcome.unchanged && self.last_cursor == cursor && self.pushed {
            // Neither buffer nor cursor has changed since the last redraw, and zsh
            // already has the highlights. Nothing to do.
            return;
        }
        self.last_cursor = cursor;

        // The spans that are published are not the lexer's spans: two of the
        // styles cannot be decided by the lexer. They are built here.
        //
        // The fields are taken apart so that the tokens — which borrow the
        // lexer — and the buffers that are written to do not borrow each
        // other.
        let Highlighter {
            cache,
            spans,
            paths,
            scratch,
            ..
        } = self;
        let tokens = cache.tokens();
        // The path lookups are timed from the start of the keystroke, so
        // that every path of this keystroke is judged by the same clock.
        scratch.now = started;
        build_spans(spans, paths, scratch, api, buffer, tokens, answers);

        // Overlay bracket and quote spans (paired bracket touches and unclosed errors)
        let cursor_byte = match cursor {
            Some(c) if c >= 0 => {
                let c = c as usize;
                if buffer.is_ascii() {
                    Some(c.min(buffer.len()))
                } else {
                    char_offsets_into(&mut self.offsets, buffer);
                    Some(
                        self.offsets
                            .partition_point(|&chars| (chars as usize) < c)
                            .min(buffer.len()),
                    )
                }
            }
            _ => None,
        };
        let brackets = highlight::bracket_spans(buffer, cursor_byte);
        highlight::apply_overlays(&mut self.spans, &brackets);

        if self.spans == self.pushed_spans && self.pushed {
            // The styling has not changed, so the regions zsh holds are
            // still correct. Leaving them alone is the whole point of the
            // "skip entirely when unchanged" rule.
            return;
        }

        if !publish || !api.in_zle() {
            // `WIDGET` is created by `makezleparams`
            // (`zsh-src/Src/Zle/zle_params.c:141-188`) and exists only while
            // a widget is running. It is a *scalar*, so testing it is O(1).
            //
            // It is not, on its own, proof that the `region_highlight` the
            // module would write to is the ZLE special parameter: a user
            // could have a global `WIDGET` of their own. So the parameter
            // itself is checked as well — it has to *exist*, which a global
            // array of the same name would also satisfy, so the shell side
            // checks the type as well (see `zsh/widgets.zsh`).
            self.pushed = false;
            return;
        }

        // The parameter has to exist: writing to a name that does not exist
        // would create a global parameter of that name in the user's shell.
        if !self.region_highlight_exists(api) {
            self.pushed = false;
            return;
        }

        // The byte-to-character map is only needed when the line is not
        // pure ASCII: for ASCII, byte offsets *are* character offsets, and
        // building the map would cost more than the whole lexer.
        let ascii = buffer.is_ascii();
        if !ascii {
            char_offsets_into(&mut self.offsets, buffer);
        }
        let offsets = if ascii {
            None
        } else {
            Some(self.offsets.as_slice())
        };

        if *debug_enabled() {
            // The diff is the "what changed" report: which ranges of the
            // line are styled differently from the last version.
            let changes = changed_ranges(&self.pushed_spans, &self.spans);
            let ranges: Vec<String> = changes
                .iter()
                .map(|c| format!("{}..{}", c.start, c.end))
                .collect();
            debug(&format!(
                "{} range(s) restyled: {}",
                changes.len(),
                ranges.join(" ")
            ));
        }

        if started.elapsed() > BUDGET {
            // The line is too expensive to highlight right now. The cache
            // has been updated, so the next keystroke will be cheap; this
            // one is simply left unhighlighted rather than highlighted late.
            //
            // The highlights on screen describe the previous line, so they
            // are cleared rather than left to lie about the current one.
            if *debug_enabled() {
                debug("highlighting exceeded the budget; skipping this redraw");
            }
            self.clear(api, publish);
            return;
        }

        // Merge with whatever another plugin has published.
        //
        // `region_highlight` is replaced wholesale by `setaparam`, so
        // publishing only our own entries would delete everyone else's. The
        // entries we published last time are recognised by their `memo=` and
        // are replaced; everything else is kept.
        let mut existing: Vec<String> = Vec::new();
        let known = api.get_param_array(c"region_highlight", &mut existing);
        if !known {
            // The parameter is not there, so there is nothing to merge with
            // and nothing to publish into: writing would create a *global*
            // parameter of that name in the user's shell.
            self.pushed = false;
            self.has_foreign_regions = false;
            return;
        }
        self.has_foreign_regions = existing.iter().any(|e| !region::is_our_entry(e));
        let (rows, truncated) = self.entries.build_merged(&existing, &self.spans, offsets);
        if *debug_enabled() {
            debug(&format!("published {} regions", rows.len()));
        }
        if truncated && *debug_enabled() {
            debug("too many highlighted regions; the highlighting was truncated");
        }
        api.set_param_array(c"region_highlight", &rows);

        // The spans that were just published become the reference for the
        // next comparison; the old ones become the scratch.
        core::mem::swap(&mut self.spans, &mut self.pushed_spans);
        self.pushed = true;
    }

    /// Whether `region_highlight` exists, computed at most once per line.
    fn region_highlight_exists(&mut self, api: &Api) -> bool {
        if let Some(known) = self.region_highlight_exists {
            return known;
        }
        let exists = api.param_exists(c"region_highlight");
        self.region_highlight_exists = Some(exists);
        exists
    }

    /// Forget what was published, without touching the display.
    ///
    /// zsh empties the display array at the end of every line
    /// (`zsh-src/Src/Zle/zle_main.c:1398`), so the module must not believe
    /// it still holds the regions it published for the previous line.
    fn forget(&mut self) {
        self.pushed = false;
        self.spans.clear();
        self.pushed_spans.clear();
        self.last_cursor = None;
        self.has_foreign_regions = false;
        // A new line has a new parameter scope, so the cached answer about
        // `region_highlight` no longer applies.
        self.region_highlight_exists = None;
    }

    /// Remove every highlight from the display.
    ///
    /// Only our own `memo=zsh-pulse` entries are removed; regions published by
    /// other plugins must remain visible on bailout/clear paths.
    fn clear(&mut self, api: &Api, publish: bool) {
        // The buffers are emptied, not replaced: a bail-out is usually
        // followed by an ordinary keystroke, and that keystroke must not have
        // to reallocate.
        self.spans.clear();
        self.pushed_spans.clear();
        self.entries.clear();
        self.last_cursor = None;
        let had_published = self.pushed;
        self.pushed = false;
        if !had_published {
            // Nothing was ever published, so there is nothing to clear.
            self.has_foreign_regions = false;
            return;
        }
        if !publish || !api.in_zle() {
            // We are not publishing; leave foreign-state tracking to the next
            // successful publish/read of the parameter.
            self.has_foreign_regions = false;
            return;
        }
        if !self.has_foreign_regions {
            // No foreign entries needed preservation on the last publish, so
            // clearing our regions can avoid reading/merging the whole array.
            api.set_param_array(c"region_highlight", &[]);
            self.has_foreign_regions = false;
            return;
        }
        // Preserve foreign (non-ours) entries instead of wiping the whole array.
        let mut existing: Vec<String> = Vec::new();
        let known = api.get_param_array(c"region_highlight", &mut existing);
        if !known {
            self.has_foreign_regions = false;
            return;
        }
        let foreign = region::merge_entries(&existing, &[]);
        let foreign_refs: Vec<&std::ffi::CStr> = foreign.iter().map(|s| s.as_c_str()).collect();
        api.set_param_array(c"region_highlight", &foreign_refs);
        self.has_foreign_regions = false;
    }
}

// ---------------------------------------------------------------------------
// The command words
// ---------------------------------------------------------------------------

/// What the shell said about a command word.
///
/// The shell — not the module — knows whether a word is an alias, a
/// function, a builtin, a command on `$PATH` or a reserved word: those are
/// the shell's own tables. The adapter looks the word up and passes the
/// answer to the module in the arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandKnowledge {
    /// The shell knows the word.
    Known,
    /// The shell does not know the word.
    Unknown,
    /// A reserved word: `if`, `then`, `for`, `while`, etc.
    Reserved,
    /// A global alias.
    GlobalAlias,
    /// A recognized subcommand in argument position.
    Subcommand,
}

/// One answer from the shell: what it said about one command word.
///
/// The word is part of the answer: the module only applies an answer to
/// the word it matches, byte for byte. If the shell and the lexer do not
/// agree on where the command word is, the answer is dropped rather than
/// applied to the wrong word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandAnswer {
    /// Where the word starts in the answers' word buffer.
    start: u32,
    /// How long the word is, in bytes.
    len: u32,
    /// What the shell said about the word.
    knowledge: CommandKnowledge,
}

impl CommandAnswer {
    /// The word this answer is about.
    ///
    /// The word lives in the answers' own buffer, so the answer does not
    /// point at anything: the words are copied out of zsh's argument
    /// vector when they are read.
    fn word<'a>(&self, words: &'a [u8]) -> &'a [u8] {
        let start = self.start as usize;
        let end = start + self.len as usize;
        words.get(start..end).unwrap_or(&[])
    }
}

/// The shell's answers about the command words of the line.
///
/// A line can hold more than one command word — `ls | wc -l` has two —
/// so the answers are a **list**. Each answer carries the word it is
/// about, the raw text of the word as the user typed it, and the module
/// applies an answer only to the word that matches it exactly.
///
/// The words are kept in one buffer, back to back, and the answers refer
/// to ranges in it: the buffer is reused between keystrokes, so a
/// keystroke does not allocate.
#[derive(Debug, Default)]
struct CommandAnswers {
    /// The words the shell was asked about, back to back.
    words: Vec<u8>,
    /// The answers, as ranges into `words`.
    answers: Vec<CommandAnswer>,
}

impl CommandAnswers {
    /// No answers at all.
    const fn new() -> Self {
        Self {
            words: Vec::new(),
            answers: Vec::new(),
        }
    }

    /// Forget every answer.
    ///
    /// The buffers keep their capacity: the answers of one keystroke are
    /// replaced by the answers of the next, and a reallocation on the
    /// interactive thread is exactly what the scratch exists to avoid.
    fn clear(&mut self) {
        self.words.clear();
        self.answers.clear();
    }

    /// Record what the shell said about a word.
    ///
    /// The word is copied into the buffer, so the answer does not point at
    /// zsh's argument vector.
    ///
    /// The copy is bounded: the words come from the line, and the line may
    /// be long. The number of answers is bounded too — see
    /// [`MAX_ARGUMENTS`] — but the *bytes* they take were not, and a
    /// pasted line of a megabyte would have been copied into the module's
    /// own buffer and kept there for the rest of the session.
    fn push(&mut self, word: &[u8], knowledge: CommandKnowledge) {
        if self.words.len() + word.len() > MAX_ANSWER_BYTES {
            // The answer does not fit: it is dropped, and the command word
            // it was about is left alone. That is the same outcome as an
            // answer the shell never gave, and it is a *missing* colour,
            // never a wrong one.
            return;
        }
        // The buffer holds the words of one keystroke, so it is small: the
        // conversion cannot fail in practice, and a word that somehow did
        // not fit would simply never match.
        let start = u32::try_from(self.words.len()).unwrap_or(u32::MAX);
        self.words.extend_from_slice(word);
        self.answers.push(CommandAnswer {
            start,
            len: u32::try_from(word.len()).unwrap_or(u32::MAX),
            knowledge,
        });
    }

    /// What the shell said about this word.
    ///
    /// `None` when the shell said nothing about it. That is not the same
    /// as "the shell does not know it": an unanswered command word is
    /// left exactly as the user typed it, while an unknown one is
    /// painted red.
    ///
    /// The first answer about a word wins. The shell does not contradict
    /// itself, and an answer that contradicts an earlier one would be
    /// about a different word anyway.
    fn knowledge_of(&self, word: &[u8]) -> Option<CommandKnowledge> {
        self.answers
            .iter()
            .find(|answer| answer.word(&self.words) == word)
            .map(|answer| answer.knowledge)
    }

    /// Read the shell's answers out of the argument vector.
    ///
    /// The protocol is a list: `--command-known=<word>` says the shell
    /// knows the word, `--command-unknown=<word>` says it does not, and
    /// both may be repeated — one answer for each command word of the
    /// line.
    ///
    /// The word follows the `=` so that a word with a space in it is
    /// still one argument, and it is the raw text of the word as the
    /// user typed it: the module compares it with the word it found,
    /// byte for byte.
    ///
    /// The words are copied out of zsh's argument vector rather than
    /// pointed at: the vector belongs to zsh, and the keystroke path
    /// must not allocate — and the answers must not outlive the call
    /// they belong to.
    fn set_from_args(&mut self, args: *mut *mut std::ffi::c_char) {
        self.clear();
        if args.is_null() {
            return;
        }
        // Safety: args is passed directly by zsh to the builtin handler.
        let iter = unsafe { crate::RawArgIter::new(args) };
        for arg in iter {
            let bytes = arg.to_bytes();
            if let Some(word) = bytes.strip_prefix(COMMAND_KNOWN_ARGUMENT) {
                self.push(word, CommandKnowledge::Known);
            } else if let Some(word) = bytes.strip_prefix(COMMAND_UNKNOWN_ARGUMENT) {
                self.push(word, CommandKnowledge::Unknown);
            } else if let Some(word) = bytes.strip_prefix(COMMAND_RESERVED_ARGUMENT) {
                self.push(word, CommandKnowledge::Reserved);
            } else if let Some(word) = bytes.strip_prefix(COMMAND_GLOBAL_ALIAS_ARGUMENT) {
                self.push(word, CommandKnowledge::GlobalAlias);
            } else if let Some(word) = bytes.strip_prefix(COMMAND_SUBCOMMAND_ARGUMENT) {
                self.push(word, CommandKnowledge::Subcommand);
            }
        }
    }
}

/// The argument that says the shell knows the command word.
///
/// The word follows the `=`, so that a word with a space in it is still
/// one argument.
const COMMAND_KNOWN_ARGUMENT: &[u8] = b"--command-known=";

/// The argument that says the shell does not know the command word.
const COMMAND_UNKNOWN_ARGUMENT: &[u8] = b"--command-unknown=";

/// The argument that says the command word is a reserved word.
const COMMAND_RESERVED_ARGUMENT: &[u8] = b"--command-reserved=";

/// The argument that says the command word is a global alias.
const COMMAND_GLOBAL_ALIAS_ARGUMENT: &[u8] = b"--command-global-alias=";

/// The argument that says the word is a recognized subcommand.
const COMMAND_SUBCOMMAND_ARGUMENT: &[u8] = b"--subcommand=";

/// What to do with a word.
///
/// The three cases are genuinely different, and conflating them is a
/// bug. A word that keeps the lexer's style is *styled*; a word with no
/// region at all is left exactly as the user typed it. The difference
/// is visible: the lexer's own style for a command word is
/// [`StyleId::Command`], which is green, and green asserts that the
/// shell knows the command — an assertion that was never established
/// for a word the shell said nothing about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordStyle {
    /// The word is styled with this style.
    Styled(StyleId),
    /// The word keeps the style the lexer gave it.
    AsLexed,
    /// The word gets no region at all.
    Unstyled,
}

// ---------------------------------------------------------------------------
// Building the spans
// ---------------------------------------------------------------------------

/// Build the styled spans of the line.
///
/// The lexer says what each word *is*. Two things it cannot say are
/// answered here:
///
/// - whether the shell knows the command word. That is the shell's
///   business, and the shell says so in the arguments;
/// - whether an argument names a file that exists. That needs the
///   filesystem, so only a word that could be a path is looked up, and the
///   answer is remembered.
///
/// # Safety
///
/// Must be called inside zsh: the shell's directory and home directory are
/// read from the shell's parameters.
fn build_spans(
    spans: &mut Vec<HighlightSpan>,
    paths: &mut PathCache,
    scratch: &mut PathScratch,
    api: &Api,
    buffer: &[u8],
    tokens: &[Token],
    answers: &CommandAnswers,
) {
    // The shell's directory is read again on every keystroke: the user may
    // have changed directory since the last one. It is read at most once
    // per keystroke, and only when a path actually needs it.
    scratch.cwd_read = false;
    scratch.home_read = false;

    // The path lookup is the only part that needs the shell, so it is
    // handed to the core as a closure. The core — the part that decides
    // what a command word looks like — is then free of the shell, and can
    // be exercised on its own.
    //
    // The number of lookups is bounded per line: every literal word is a
    // candidate for being a file name, and a long line would otherwise
    // cost one `stat` per word. The words are looked up in the order they
    // appear, so the ones nearest the command — the ones most likely to
    // be paths — are the ones that are looked up.
    let mut lookups = 0usize;
    let mut path = |word: &[Token]| {
        if lookups >= MAX_PATH_LOOKUPS {
            return None;
        }
        // The budget is checked *before* the lookup, not only after the
        // whole line is done: a `stat` on a network filesystem can take
        // milliseconds, and the answer to a line that took too long is
        // thrown away. Stopping early turns "the user waits and gets
        // nothing" into "the user waits only as long as the budget".
        if scratch.now.elapsed() > BUDGET {
            return None;
        }
        lookups += 1;
        path_style(paths, scratch, api, word, buffer)
    };
    build_spans_with(spans, buffer, tokens, answers, &mut path);
}

/// How many paths are looked up on one line.
///
/// Every literal word can name a file, and the reference implementation
/// looks every one of them up
/// (`research/upstream/fsh/fast-highlight:1265-1295`). A `stat` costs
/// about a microsecond, so a line with many arguments would cost a
/// noticeable pause on the interactive thread.
///
/// The bound keeps the cost of one keystroke predictable: sixteen lookups
/// is about fifteen microseconds, well inside the millisecond the
/// keystroke has to fit in. The words are looked up in the order they
/// appear, so the ones that are looked up are the ones nearest the
/// command.
const MAX_PATH_LOOKUPS: usize = 16;

/// Build the styled spans of the line, given how to style an argument.
///
/// `path_style` answers the one question the lexer cannot: what an
/// argument that names a file looks like. `None` means that the argument
/// does not name anything that exists, and it keeps the lexer's style.
///
/// Everything else — in particular what a *command* word looks like — is
/// decided from `answers`, the shell's own words about the command words
/// of the line.
fn build_spans_with<F>(
    spans: &mut Vec<HighlightSpan>,
    buffer: &[u8],
    tokens: &[Token],
    answers: &CommandAnswers,
    path_style: &mut F,
) where
    F: FnMut(&[Token]) -> Option<StyleId>,
{
    spans.clear();

    let mut i = 0;
    let mut after_double_dash = false;
    while i < tokens.len() {
        let token = tokens[i];
        if !token.kind.is_word_part() {
            // A command separator or pipe starts a new command context.
            if token.kind == TokenKind::Operator {
                after_double_dash = false;
            }
            // Not a word: an operator, a redirection, a comment or a
            // here-document. It keeps the style the lexer gave it.
            push_span(spans, token.start, token.end, StyleId::of(token.kind));
            i += 1;
            continue;
        }

        // The word: a run of tokens that touch each other. The space
        // between two words is what tells them apart.
        let start = i;
        let mut end = i + 1;
        while end < tokens.len()
            && tokens[end].kind.is_word_part()
            && tokens[end - 1].end == tokens[end].start
        {
            end += 1;
        }
        let word = &tokens[start..end];
        let text = word_text(word, buffer);

        let is_cmd = is_command_word(word, buffer);
        if is_cmd {
            after_double_dash = false;
        } else if text == b"--" {
            after_double_dash = true;
        }

        // A command word is styled from the shell's answers; every other
        // word is an argument, and may name a file that exists. A recognized
        // subcommand answer can override an argument-position word (adapter
        // identifies subcommand position).
        // Words after `--` are treated as operands rather than options.
        let subcommand = if !is_cmd && text != b"--" {
            matches!(
                answers.knowledge_of(text),
                Some(CommandKnowledge::Subcommand)
            )
        } else {
            false
        };
        let style = if is_cmd {
            command_style(word, buffer, answers, path_style)
        } else if subcommand {
            WordStyle::Styled(StyleId::Subcommand)
        } else if after_double_dash && text != b"--" {
            match path_style(word) {
                Some(style) => WordStyle::Styled(style),
                None => WordStyle::Styled(StyleId::Argument),
            }
        } else {
            match path_style(word) {
                Some(style) => WordStyle::Styled(style),
                None => WordStyle::AsLexed,
            }
        };

        push_word(spans, word, buffer, answers, style);
        i = end;
    }
}

/// The spans the module publishes for a line.
///
/// The command words are styled from `answers`, the way the shell's own
/// answer styles them: `--command-known=<word>` says the shell knows the
/// word and `--command-unknown=<word>` says it does not. A command word
/// the answers say nothing about gets no span at all.
///
/// The paths are **not** looked up: that needs the shell's directory, and
/// this function has no shell. A word that names a file is therefore
/// styled as the lexer styled it.
///
/// This is what the benchmark measures and what the tests inspect: the
/// spans the module really publishes, not a stand-in. A line that does
/// not scan has no spans.
pub fn published_spans(text: &[u8], answers: &[&[u8]]) -> Vec<HighlightSpan> {
    let Ok(lexed) = lexer::lex(text) else {
        return Vec::new();
    };
    let mut command_answers = CommandAnswers::new();
    for answer in answers {
        if let Some(word) = answer.strip_prefix(COMMAND_KNOWN_ARGUMENT) {
            command_answers.push(word, CommandKnowledge::Known);
        } else if let Some(word) = answer.strip_prefix(COMMAND_UNKNOWN_ARGUMENT) {
            command_answers.push(word, CommandKnowledge::Unknown);
        } else if let Some(word) = answer.strip_prefix(COMMAND_RESERVED_ARGUMENT) {
            command_answers.push(word, CommandKnowledge::Reserved);
        } else if let Some(word) = answer.strip_prefix(COMMAND_GLOBAL_ALIAS_ARGUMENT) {
            command_answers.push(word, CommandKnowledge::GlobalAlias);
        } else if let Some(word) = answer.strip_prefix(COMMAND_SUBCOMMAND_ARGUMENT) {
            command_answers.push(word, CommandKnowledge::Subcommand);
        }
    }
    let mut spans = Vec::new();
    build_spans_with(
        &mut spans,
        text,
        &lexed.tokens,
        &command_answers,
        &mut |_word| None,
    );
    spans
}

/// Publish the spans of one word.
///
/// `style` is what the word as a whole gets. The exception is a token
/// inside the word that is a command in its own right — the `cmd` of
/// `$(cmd)`, say: such a token is styled from the shell's answers like
/// any other command, and gets no region at all when the shell said
/// nothing about it.
fn push_word(
    spans: &mut Vec<HighlightSpan>,
    word: &[Token],
    buffer: &[u8],
    answers: &CommandAnswers,
    style: WordStyle,
) {
    for token in word {
        let token_style = match style {
            WordStyle::Styled(style) => Some(style),
            WordStyle::AsLexed => {
                if token.kind == TokenKind::Command {
                    // A command inside a word: the command of a command
                    // substitution. It is a command in its own right, so
                    // it is styled from the shell's answers — and the
                    // lexer's green is not published for a command
                    // nobody said anything about.
                    command_token_style(token, buffer, answers)
                } else {
                    Some(StyleId::of(token.kind))
                }
            }
            WordStyle::Unstyled => None,
        };
        if let Some(style) = token_style {
            push_span(spans, token.start, token.end, style);
        }
    }
}

/// The style of one command token, given what the shell said about it.
///
/// A command token with no answer gets no style at all: the word it is
/// in keeps the style the lexer gave it, and the command itself is left
/// alone.
fn command_token_style(token: &Token, buffer: &[u8], answers: &CommandAnswers) -> Option<StyleId> {
    match answers.knowledge_of(token_bytes(token, buffer)) {
        Some(CommandKnowledge::Known) => Some(StyleId::Command),
        Some(CommandKnowledge::Subcommand) => Some(StyleId::Subcommand),
        Some(CommandKnowledge::Reserved) => Some(StyleId::ReservedWord),
        Some(CommandKnowledge::GlobalAlias) => Some(StyleId::GlobalAlias),
        Some(CommandKnowledge::Unknown) => Some(StyleId::UnknownCommand),
        None => None,
    }
}

/// Whether the word is a command word.
///
/// A word is a command word when the lexer says so. The one case the
/// lexer cannot say plainly is a word that starts with a tilde: the
/// scanner emits the `~` as a glob, because that is what it is in a word
/// like `~/*.txt`, and the rest of the word keeps the kind the word has.
/// `~/bin/tool` is therefore a glob followed by a command, and it is a
/// command word like any other.
///
/// A word is *not* a command word when the command is somewhere inside
/// it — the `cmd` of `$(cmd)` is a command, but the word `$(cmd)` is a
/// substitution.
fn is_command_word(word: &[Token], buffer: &[u8]) -> bool {
    let Some(first) = word.first() else {
        return false;
    };
    if first.kind == TokenKind::Command {
        return true;
    }
    // A `~` at the start of a word is a tilde expansion rather than a
    // glob, and the rest of the word is still the command.
    first.kind == TokenKind::Glob
        && token_bytes(first, buffer) == b"~"
        && word.iter().any(|t| t.kind == TokenKind::Command)
}

/// The style of a command word, given what the shell said about it.
///
/// A command word the shell knows is green; one the shell does not know
/// is red and bold, the way the reference implementation colours an
/// unknown token (`research/upstream/fsh/fast-highlight:58-93`).
///
/// A command word the shell said **nothing** about gets no region at
/// all. The lexer's own style for a command word is green, and green
/// asserts that the shell knows the command — an assertion that was
/// never established. Publishing it would paint the `nosuchcmd` of
/// `ls | nosuchcmd` green, which is what the module used to do.
///
/// A word the shell does not know may still name a file that exists —
/// `~/bin/tool` is a path, not a typo — so the filesystem is consulted
/// before the word is called unknown. The reference implementation does
/// the same: it looks the word up as a command first and only then
/// checks whether it is a path.
fn command_style<F>(
    word: &[Token],
    buffer: &[u8],
    answers: &CommandAnswers,
    path_style: &mut F,
) -> WordStyle
where
    F: FnMut(&[Token]) -> Option<StyleId>,
{
    // The answer is about one word. If it is not the word the lexer
    // found, it is dropped: colouring the wrong word red would be worse
    // than not colouring it at all.
    match answers.knowledge_of(word_text(word, buffer)) {
        Some(CommandKnowledge::Known) => WordStyle::Styled(StyleId::Command),
        Some(CommandKnowledge::Subcommand) => WordStyle::Styled(StyleId::Subcommand),
        Some(CommandKnowledge::Reserved) => WordStyle::Styled(StyleId::ReservedWord),
        Some(CommandKnowledge::GlobalAlias) => WordStyle::Styled(StyleId::GlobalAlias),
        Some(CommandKnowledge::Unknown) => match path_style(word) {
            Some(style) => WordStyle::Styled(style),
            None => WordStyle::Styled(StyleId::UnknownCommand),
        },
        None => match path_style(word) {
            Some(style) => WordStyle::Styled(style),
            None => WordStyle::Unstyled,
        },
    }
}

/// The style of a word that could name a file.
///
/// `None` when the word does not name a file, or does not name anything
/// that exists.
///
/// Every literal word is a candidate: `Cargo.toml` names a file just as
/// much as `./Cargo.toml` does, and the reference implementation looks
/// every word up (`research/upstream/fsh/fast-highlight:1265-1295`). The
/// number of lookups is bounded per line — see [`MAX_PATH_LOOKUPS`].
///
/// # Safety
///
/// Must be called inside zsh: the shell's directory and home directory are
/// read from the shell's parameters.
fn path_style(
    paths: &mut PathCache,
    scratch: &mut PathScratch,
    api: &Api,
    word: &[Token],
    buffer: &[u8],
) -> Option<StyleId> {
    // The time of the keystroke, which is when the path is looked up.
    let now = scratch.now;
    // Only a word that is literal text can name a file. A word with a
    // quote or an expansion in it names whatever the shell makes of it, and
    // that is not knowable without running the shell.
    if !is_literal(word, buffer) {
        return None;
    }
    let text = word_text(word, buffer);

    // Resolve the word against the shell's directory.
    scratch.path.clear();
    if let Some(rest) = text.strip_prefix(b"~") {
        // `~` and `~/...` expand to the home directory. `~user` needs a
        // password database lookup, which is not worth doing on the
        // interactive thread.
        if !rest.is_empty() && !rest.starts_with(b"/") {
            return None;
        }
        read_home(api, scratch);
        if scratch.home.is_empty() {
            // The shell has no `$HOME`, so the word does not name a path
            // that can be looked up.
            return None;
        }
        scratch.path.extend_from_slice(&scratch.home);
        scratch.path.extend_from_slice(rest);
    } else if text.starts_with(b"/") {
        scratch.path.extend_from_slice(text);
    } else {
        // A relative path is relative to the shell's directory.
        read_cwd(api, scratch);
        if scratch.cwd.is_empty() {
            // The shell has no `$PWD`, so a relative path cannot be
            // resolved.
            return None;
        }
        scratch.path.extend_from_slice(&scratch.cwd);
        scratch.path.push(b'/');
        scratch.path.extend_from_slice(text);
    }

    match paths.lookup(&scratch.path, now) {
        Some(PathKind::Dir) => Some(StyleId::PathToDir),
        Some(PathKind::File) => Some(StyleId::Path),
        None => None,
    }
}

/// Whether the word is literal text, so that it can name a file.
///
/// A word with a quote or an expansion in it is not literal: the file it
/// names is not knowable without running the shell, so it is left alone.
fn is_literal(word: &[Token], buffer: &[u8]) -> bool {
    let mut rest = word.iter();
    let Some(first) = rest.next() else {
        return false;
    };
    // A `Command` token is plain text as well: it is how the lexer spells
    // the words that stand in command position, and the text of such a
    // word is literal.
    let plain = |t: &Token| t.kind == TokenKind::Word || t.kind == TokenKind::Command;
    if first.kind == TokenKind::Word || first.kind == TokenKind::Command {
        return rest.all(plain);
    }
    // A `~` at the start of a word is a tilde expansion rather than a
    // glob, and the rest of the word is still literal text.
    first.kind == TokenKind::Glob && token_bytes(first, buffer) == b"~" && rest.all(plain)
}

/// The bytes of the word.
///
/// The tokens of a word touch each other, so the word is the slice of the
/// line they cover.
fn word_text<'a>(word: &[Token], buffer: &'a [u8]) -> &'a [u8] {
    let Some(first) = word.first() else {
        return &[];
    };
    let Some(last) = word.last() else {
        return &[];
    };
    buffer
        .get(first.start as usize..last.end as usize)
        .unwrap_or(&[])
}

/// The bytes of one token.
fn token_bytes<'a>(token: &Token, buffer: &'a [u8]) -> &'a [u8] {
    buffer
        .get(token.start as usize..token.end as usize)
        .unwrap_or(&[])
}

/// Add a span, merging it with the previous one when they have the same
/// style and touch each other.
///
/// The merge is what keeps the number of regions proportional to the
/// number of *visual* runs: a double-quoted string is three tokens but one
/// run.
fn push_span(spans: &mut Vec<HighlightSpan>, start: u32, end: u32, style: StyleId) {
    if end <= start {
        return;
    }
    match spans.last_mut() {
        Some(last) if last.style == style && last.end == start => last.end = end,
        _ => spans.push(HighlightSpan::new(start, end, style)),
    }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// What a path turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathKind {
    /// A file that exists.
    File,
    /// A directory that exists.
    Dir,
}

/// One remembered answer about a path.
#[derive(Debug)]
struct PathEntry {
    /// The resolved path.
    path: Box<[u8]>,
    /// What it is, if it exists.
    kind: Option<PathKind>,
    /// When the answer was made.
    ///
    /// The answer goes stale: a file that is created while the user is
    /// typing must be picked up, and a file that is deleted must stop
    /// being highlighted. The entry is therefore only trusted for
    /// [`PATH_TTL`].
    looked_up: Instant,
}

/// The answers to "does this path exist, and what is it?".
///
/// The module is on the interactive thread and the same path is looked up
/// on every keystroke, so the answers are remembered.
///
/// The cache is keyed by the *resolved* path, so a change of directory
/// cannot make a stale answer look current: the same word resolves to a
/// different path, and is therefore looked up again.
///
/// Every answer has a **time to live**. Without one, a file that did not
/// exist when it was typed would stay unhighlighted for the rest of the
/// session — even after the user created it — and a file that was deleted
/// would stay highlighted. Both are visible bugs, and both are fixed by
/// looking the path up again once the answer has grown old.
#[derive(Debug, Default)]
struct PathCache {
    /// The answers, in the order they were found.
    entries: Vec<PathEntry>,
}

/// How many paths are remembered.
///
/// A session can see a lot of paths, and the cache must not grow without
/// bound. When it is full it is emptied, which costs one `stat` per path
/// again and nothing more.
const MAX_CACHED_PATHS: usize = 64;

/// How long an answer about a path is trusted.
///
/// Two seconds is short enough that a file the user has just created is
/// picked up while they are still typing the line that names it, and long
/// enough that the repeated lookups of a single line — one per keystroke —
/// are served from the cache.
///
/// The lookup itself is a `stat(2)`: it does not fork and it does not
/// block for long, so looking a path up again is cheap. What the cache
/// saves is the *number* of them, not their cost.
const PATH_TTL: Duration = Duration::from_secs(2);

impl PathCache {
    /// What the path is, asking the filesystem only the first time.
    ///
    /// `None` means that nothing exists at that path.
    ///
    /// The answer is only trusted for [`PATH_TTL`]: after that the path
    /// is looked up again, so a file that appears — or disappears — while
    /// the user is typing is picked up.
    ///
    /// `now` is the time of the keystroke. It is passed in rather than
    /// read here so that the whole keystroke shares one clock reading,
    /// and so that the expiry can be tested without waiting for it.
    fn lookup(&mut self, path: &[u8], now: Instant) -> Option<PathKind> {
        if let Some(entry) = self.entries.iter_mut().find(|e| &*e.path == path) {
            if now.duration_since(entry.looked_up) < PATH_TTL {
                // The answer is fresh enough to be trusted.
                return entry.kind;
            }
            // The answer is too old to be trusted: the file may have been
            // created or deleted since it was made.
            let kind = stat_path(path);
            entry.kind = kind;
            entry.looked_up = now;
            return kind;
        }
        let kind = stat_path(path);
        if self.entries.len() >= MAX_CACHED_PATHS {
            self.entries.clear();
        }
        self.entries.push(PathEntry {
            path: Box::from(path),
            kind,
            looked_up: now,
        });
        kind
    }
}

/// What is at this path, if anything.
///
/// The path is used as raw bytes: a file name is not necessarily valid
/// UTF-8, and the module must not mangle it. `OsStr::from_bytes` is the
/// Unix way of saying exactly that.
fn stat_path(path: &[u8]) -> Option<PathKind> {
    let os = std::ffi::OsStr::from_bytes(path);
    match std::fs::metadata(std::path::Path::new(os)) {
        Ok(meta) if meta.is_dir() => Some(PathKind::Dir),
        Ok(_) => Some(PathKind::File),
        Err(_) => None,
    }
}

/// The scratch the path lookups need.
///
/// Reused between keystrokes: a lookup must not allocate on the
/// interactive thread.
#[derive(Debug)]
struct PathScratch {
    /// The resolved path.
    path: Vec<u8>,
    /// The shell's current directory.
    cwd: Vec<u8>,
    /// The shell's home directory.
    home: Vec<u8>,
    /// Whether the current directory has been read.
    cwd_read: bool,
    /// Whether the home directory has been read.
    home_read: bool,
    /// The time of the keystroke.
    ///
    /// The path cache uses it to decide whether an answer has grown old:
    /// one clock reading for the whole keystroke, and the same reading
    /// for every path of that keystroke.
    now: Instant,
}

impl Default for PathScratch {
    fn default() -> Self {
        Self {
            path: Vec::new(),
            cwd: Vec::new(),
            home: Vec::new(),
            cwd_read: false,
            home_read: false,
            now: Instant::now(),
        }
    }
}

/// Read the shell's current directory into the scratch.
///
/// # Safety
///
/// Must be called inside zsh.
fn read_cwd(api: &Api, scratch: &mut PathScratch) {
    if scratch.cwd_read {
        return;
    }
    scratch.cwd_read = true;
    let read = api.read_param_bytes(c"PWD", &mut scratch.cwd);
    if read.is_none() {
        // The shell has no `$PWD`.
        scratch.cwd.clear();
    }
}

/// Read the shell's home directory into the scratch.
fn read_home(api: &Api, scratch: &mut PathScratch) {
    if scratch.home_read {
        return;
    }
    scratch.home_read = true;
    let read = api.read_param_bytes(c"HOME", &mut scratch.home);
    if read.is_none() {
        // The shell has no `$HOME`.
        scratch.home.clear();
    }
}

// ---------------------------------------------------------------------------
// What changed
// ---------------------------------------------------------------------------

/// The ranges whose styling differs between two versions of a line.
///
/// The `highlight` crate has a diff of its own, but it compares the
/// *lexer's* spans. The spans that are published are not the lexer's — the
/// command word and the paths are restyled after the lexing — so the diff
/// has to be done here to say what actually changed on the screen.
fn changed_ranges(old: &[HighlightSpan], new: &[HighlightSpan]) -> Vec<ChangedRange> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut pos = 0u32;
    loop {
        let so = style_at(old, i, pos);
        let sn = style_at(new, j, pos);

        // The end of the segment over which both styles are constant: the
        // first of "the current span ends" and "the next span starts",
        // whichever comes first. A span that has not started yet marks the
        // end of a gap.
        let mut end = u32::MAX;
        for (spans, idx) in [(old, i), (new, j)] {
            if let Some(s) = spans.get(idx) {
                if s.start > pos {
                    end = end.min(s.start);
                }
                end = end.min(s.end);
            }
        }
        if end == u32::MAX || end <= pos {
            break;
        }

        if so != sn {
            out.push(ChangedRange {
                start: pos,
                end,
                style: sn.unwrap_or(StyleId::Argument),
            });
        }
        pos = end;
        while old.get(i).is_some_and(|s| s.end <= pos) {
            i += 1;
        }
        while new.get(j).is_some_and(|s| s.end <= pos) {
            j += 1;
        }
    }
    out
}

/// The style of the span at `idx` that covers `offset`.
fn style_at(spans: &[HighlightSpan], idx: usize, offset: u32) -> Option<StyleId> {
    let s = spans.get(idx)?;
    if s.start <= offset && offset < s.end {
        Some(s.style)
    } else {
        None
    }
}

/// The process-wide highlighter.
static HIGHLIGHTER: std::sync::LazyLock<std::sync::Mutex<Highlighter>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Highlighter::default()));

/// Reusable scratch buffers for the keystroke path.
///
/// Allocated once, then reused. This is what removed the per-keystroke
/// allocations that made the first implementation 10x slower than the C
/// proof-of-concept (1.2 us vs 112 ns per call).
///
/// Reused, but not for ever: the buffer only ever holds a line that the
/// module is willing to highlight — at most [`MAX_LINE`] bytes — and
/// capacity beyond that is handed back (see
/// [`Scratch::release_unused_capacity`]).
struct Scratch {
    /// Unmetafied `BUFFER` bytes.
    ///
    /// Empty when the line is longer than [`MAX_LINE`]: such a line is not
    /// highlighted, so its bytes are not needed and are not copied.
    buffer: Vec<u8>,
    /// Raw bytes of `CURSOR`, kept only for diagnostics.
    ///
    /// Bounded by the number of decimal digits in a cursor position, so
    /// this one cannot grow without bound.
    cursor_bytes: Vec<u8>,
    /// The length of the line in zsh.
    ///
    /// Known even when the line was too long to copy, so the diagnostics
    /// can report the real size of the line.
    line_len: usize,
    /// Whether the line is longer than [`MAX_LINE`].
    ///
    /// The module does not highlight such a line, and does not copy it
    /// either: copying it is what made the scratch grow to the largest
    /// line the user had ever pasted.
    too_long: bool,
    /// What the shell said about the command words of the line.
    ///
    /// Copied out of the argument vector rather than pointed at, so that
    /// the keystroke path does not allocate — and so that the answers do
    /// not outlive the call they belong to.
    command_answers: CommandAnswers,
}

impl Scratch {
    const fn new() -> Self {
        Self {
            buffer: Vec::new(),
            cursor_bytes: Vec::new(),
            line_len: 0,
            too_long: false,
            command_answers: CommandAnswers::new(),
        }
    }

    /// Take a line into the scratch, if it is short enough to highlight.
    ///
    /// Record the line that was read from zsh.
    ///
    /// `too_long` says the line was longer than [`MAX_LINE`] and was
    /// therefore not copied: the length is still known, but the bytes are
    /// not kept, so the scratch cannot grow to the size of a paste.
    fn set_line(&mut self, len: usize, too_long: bool) {
        self.line_len = len;
        self.too_long = too_long;
        if too_long {
            self.buffer.clear();
        }
        self.release_unused_capacity();
    }

    /// Release the capacity the scratch no longer needs.
    ///
    /// `Vec::clear` keeps the capacity, so without this the shell would
    /// hold the largest line the user has ever pasted for the rest of the
    /// session.
    ///
    /// Shrinking on every keystroke would be worse than the problem it
    /// solves: `shrink_to` reallocates, and an allocation on the
    /// interactive thread is exactly what the scratch exists to avoid. The
    /// rule is therefore hysteretic — the capacity is released only when it
    /// is large in absolute terms *and* much larger than the line the
    /// scratch currently holds — so a line that grows and shrinks within
    /// the retained capacity never triggers a reallocation.
    fn release_unused_capacity(&mut self) {
        let capacity = self.buffer.capacity();
        // Keep enough room for the line itself and for one a few times
        // longer, so that ordinary editing never reallocates.
        let keep = SCRATCH_KEEP.max(self.buffer.len().saturating_mul(SCRATCH_KEEP_RATIO));
        if capacity > keep {
            self.buffer.shrink_to(SCRATCH_KEEP);
        }
    }

    /// Read `BUFFER` out of zsh into the scratch.
    ///
    /// This is `zsh_abi::Api::read_param_bytes` with a cap. That one copies
    /// the whole parameter, and the whole parameter of a pasted line is
    /// precisely what must not be copied: the module does not highlight a
    /// line longer than [`MAX_LINE`], so it needs the *length* of such a
    /// line and nothing else.
    ///
    /// `want_whole` asks for the whole line anyway, for the diagnostic
    /// modes (`ZSH_PULSE_DEBUG` and `ZSH_PULSE_ECHO`), which show the
    /// user exactly what is on the line. The copy is returned to the
    /// caller, who drops it after printing: the scratch stays small.
    ///
    /// # Safety
    ///
    /// Must be called inside zsh.
    fn read_buffer(&mut self, api: &Api, want_whole: bool) -> Option<Vec<u8>> {
        // The line is copied only if it is short enough to highlight; a
        // pasted megabyte must not be copied into the shell's memory.
        let read = api.read_param_bytes_capped(c"BUFFER", &mut self.buffer, MAX_LINE);
        match read {
            CappedRead::Missing => {
                self.set_line(0, false);
                None
            }
            CappedRead::TooLong(len) => {
                self.set_line(len, true);
                if want_whole {
                    let mut whole = Vec::new();
                    if api.read_param_bytes(c"BUFFER", &mut whole).is_some() {
                        return Some(whole);
                    }
                }
                None
            }
            CappedRead::Read(len) => {
                self.set_line(len, false);
                None
            }
        }
    }

    /// Read the shell's answers about the command words out of the arguments.
    ///
    /// The answers are copied into the scratch rather than pointed at: the
    /// argument vector belongs to zsh, and the keystroke path must not
    /// allocate.
    ///
    /// Every answer carries the word it is about. The module only applies
    /// an answer to the word it matches exactly, so a disagreement between
    /// the shell and the lexer cannot end with the wrong word coloured
    /// red.
    fn set_command_answer(&mut self, args: *mut *mut std::ffi::c_char) {
        self.command_answers.set_from_args(args);
    }
}

/// The capacity the scratch keeps for itself.
///
/// A kilobyte is more than any command line worth highlighting needs: the
/// module refuses to highlight anything longer than [`MAX_LINE`], which is
/// four times this.
const SCRATCH_KEEP: usize = 1024;

/// How much bigger than the line the capacity may be before it is released.
///
/// Four times the line is comfortably more than a command line needs, so
/// the scratch does not reallocate while the user is editing one.
const SCRATCH_KEEP_RATIO: usize = 4;

static SCRATCH: std::sync::Mutex<Scratch> = std::sync::Mutex::new(Scratch::new());

/// `zsh-pulse-highlight` — invoked once per redraw by the zsh adapter.
///
/// It reads the live ZLE state, highlights the line and hands the result to
/// zsh through `region_highlight`.
pub unsafe extern "C" fn highlight_builtin(
    _nam: *mut std::ffi::c_char,
    args: *mut *mut std::ffi::c_char,
    _ops: zsh_abi::Options,
    _len: std::ffi::c_int,
) -> std::ffi::c_int {
    guard(0, || {
        // A panic has already been caught at this boundary. Continuing would
        // panic again on the same input, and the default panic hook would
        // print a backtrace into the user's terminal on every keystroke.
        if zsh_abi::is_degraded() {
            report_degraded();
            return 0;
        }

        // Rule 1: never touch zsh before the API has been resolved.
        let Some(api) = zsh() else {
            return 0;
        };

        if !args.is_null() {
            // Safety: args is passed directly by zsh to the builtin handler.
            let iter = unsafe { crate::RawArgIter::new(args) };
            for arg in iter {
                let bytes = arg.to_bytes();
                if let Some(feat) = bytes.strip_prefix(b"--has-feature=") {
                    let has = feat == b"highlight"
                        || (feat == b"autocomplete" && cfg!(feature = "autocomplete"))
                        || ((feat == b"history" || feat == b"history-navigation")
                            && cfg!(any(feature = "history", feature = "history-navigation")));
                    return if has { 0 } else { 1 };
                }
            }
        }

        if has_argument(args, b"--is-menu-active") {
            return if api.is_menu_active() { 0 } else { 1 };
        }

        // Only the ZLE hook asks for the highlights to be published.
        let publish = wants_regions(args);

        if wants_reset(args) {
            // A new line is starting: zsh has emptied the display array, so
            // the module must forget that it published anything.
            let mut highlighter = match HIGHLIGHTER.lock() {
                Ok(h) => h,
                Err(poisoned) => poisoned.into_inner(),
            };
            highlighter.forget();
            return 0;
        }

        // Read live ZLE state into reused buffers: no allocation once warm.
        let mut guard = match SCRATCH.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        // What the shell knows about the command words of this line. The
        // shell is the only one that can know: its tables of aliases,
        // functions, builtins and commands are not visible from here.
        guard.set_command_answer(args);
        // The diagnostic modes show the user the whole line, even one that
        // is too long to highlight, so they are the only ones that pay for
        // a copy of it.
        let want_whole = *debug_enabled() || *echo_enabled();
        let (cursor, whole) = {
            let bytes = api.read_param_i64(c"CURSOR", &mut guard.cursor_bytes);
            // CURSOR must be read *before* BUFFER, because both share zsh's
            // single static unmetafied buffer.
            (bytes, guard.read_buffer(api, want_whole))
        };
        let buffer_len = guard.line_len;
        let too_long = guard.too_long;
        // The line as zsh has it: the whole of it for a line that is too
        // long to highlight, the scratch's copy otherwise.
        let line: &[u8] = whole.as_deref().unwrap_or(&guard.buffer);

        if *debug_enabled() {
            debug(&format!(
                "read BUFFER={:?} ({buffer_len} bytes) CURSOR={cursor:?}",
                String::from_utf8_lossy(line)
            ));
        }

        // Each keystroke advances the line revision; asynchronous results
        // tagged with an older revision are refused.
        let revision = engine().record_highlight();

        // Highlight the line and hand the result to zsh.
        {
            let mut highlighter = match HIGHLIGHTER.lock() {
                Ok(h) => h,
                Err(poisoned) => poisoned.into_inner(),
            };
            highlighter.update(
                api,
                &guard.buffer,
                cursor,
                too_long,
                publish,
                &guard.command_answers,
            );
        }

        #[cfg(feature = "autocomplete")]
        if wants_autosuggest(args) {
            let cwd = api
                .get_param_string(c"PWD")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
                });
            let home = api
                .get_param_string(c"HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("/home"));
            let c_val = cursor.unwrap_or(0).max(0);
            let cursor_idx = if guard.buffer.is_ascii() {
                usize::try_from(c_val).unwrap_or(0).min(guard.buffer.len())
            } else {
                let mut offsets = Vec::new();
                highlight::char_offsets_into(&mut offsets, &guard.buffer);
                offsets
                    .partition_point(|&chars| (chars as i64) < c_val)
                    .min(guard.buffer.len())
            };
            let ghost = engine().autosuggest(&guard.buffer, cursor_idx, cwd, home);
            let ghost_cstr = match ghost {
                Some(ref s) => std::ffi::CString::new(s.as_bytes()).unwrap_or_default(),
                None => std::ffi::CString::default(),
            };
            api.set_param_string(c"_zsh_pulse_autosuggestion", &ghost_cstr);
        }

        // Write-back proof. Writing a zsh parameter is currently UNSAFE on
        // repeat: `set_param_string` allocates with zsh's allocator (required,
        // since `assignaparam` frees the array it is given), but a second
        // assignment to an already-set parameter corrupts the heap — observed
        // as `free(): invalid size`. That must be understood before Stage C
        // relies on the same mechanism for `region_highlight`.
        //
        // Gated off by default so nothing unsafe runs in a normal shell.
        if *echo_enabled() {
            let value = std::ffi::CString::new(format!(
                "{revision}:{}:{}",
                cursor.unwrap_or_default(),
                String::from_utf8_lossy(line)
            ))
            .unwrap_or_default();
            api.set_param_string(c"ZSH_PULSE_STATE", &value);
        }
        0
    })
}

/// The argument that asks the module to publish the highlights.
const REGION_ARGUMENT: &[u8] = b"--region-highlight";

/// The argument that asks the module to forget what it published.
///
/// zsh empties the display array at the end of every line
/// (`zsh-src/Src/Zle/zle_main.c:1398`), so the module has to forget too —
/// otherwise a recalled command, whose text matches the cached one, would be
/// drawn with no highlighting at all.
const RESET_ARGUMENT: &[u8] = b"--reset";

/// The most bytes the answer words may take together.
///
/// The answers carry the command words of the line, and the line may be
/// long. The number of answers is bounded by [`MAX_ARGUMENTS`], but the
/// bytes they take were not: a pasted line of a megabyte would have been
/// copied into the module's own buffer — and kept there for the rest of
/// the session, because the buffer's capacity is deliberately reused.
///
/// The bound matches the line the module is willing to highlight at all
/// ([`MAX_LINE`]): an answer about a word of a longer line is of no use,
/// because the line is not highlighted.
const MAX_ANSWER_BYTES: usize = MAX_LINE;

/// Whether the caller asked for the highlights to be published.
///
/// Only the ZLE hook passes the flag. Writing to `region_highlight` from
/// anywhere else would either do nothing (the parameter only exists while
/// a widget is running) or overwrite a global array of that name that the
/// user or another plugin owns.
fn wants_reset(args: *mut *mut std::ffi::c_char) -> bool {
    has_argument(args, RESET_ARGUMENT)
}

fn wants_regions(args: *mut *mut std::ffi::c_char) -> bool {
    has_argument(args, REGION_ARGUMENT)
}

/// The builtin table handed to `addbuiltins`.
#[cfg(feature = "autocomplete")]
const AUTOSUGGEST_ARGUMENT: &[u8] = b"--autosuggest";

#[cfg(feature = "autocomplete")]
fn wants_autosuggest(args: *mut *mut std::ffi::c_char) -> bool {
    has_argument(args, AUTOSUGGEST_ARGUMENT)
}

/// The full builtin name as zsh will register it: `zsh-pulse-highlight`.
pub static BUILTIN_FULL: CStr = CStr::new(c"zsh-pulse-highlight");

#[cfg(test)]
mod tests {
    use super::*;

    // --- the scratch buffer -------------------------------------------------

    #[test]
    fn a_line_at_the_limit_is_copied() {
        let mut s = Scratch::new();
        // The read fills the buffer; the scratch records what was read.
        s.buffer.extend_from_slice(&vec![b'x'; MAX_LINE]);
        s.set_line(MAX_LINE, false);
        assert_eq!(s.buffer.len(), MAX_LINE);
        assert!(!s.too_long);
        assert_eq!(s.line_len, MAX_LINE);
    }

    #[test]
    fn a_line_that_is_too_long_is_not_copied() {
        // The whole point: a line the module will not highlight must not be
        // copied into the scratch, or the scratch grows to the largest line
        // the user has ever pasted and never gives it back.
        let mut s = Scratch::new();
        let line = vec![b'x'; MAX_LINE * 4];
        s.set_line(line.len(), true);
        assert_eq!(s.buffer.len(), 0, "the line must not be copied");
        assert!(s.too_long);
        // The length is still known: the module needs it to know that the
        // line must be skipped, and the diagnostics report it.
        assert_eq!(s.line_len, line.len());
        assert!(
            s.buffer.capacity() < line.len(),
            "the scratch must not have grown to the size of the line"
        );
    }

    #[test]
    fn a_short_line_after_a_long_one_is_highlighted_again() {
        let mut s = Scratch::new();
        s.set_line(MAX_LINE * 2, true);
        assert!(s.too_long);
        s.buffer.extend_from_slice(b"ls");
        s.set_line(2, false);
        assert!(
            !s.too_long,
            "the flag must be cleared once the line fits again"
        );
        assert_eq!(s.buffer.len(), 2);
    }

    #[test]
    fn the_scratch_releases_capacity_it_no_longer_needs() {
        let mut s = Scratch::new();
        s.buffer.extend_from_slice(&vec![b'x'; MAX_LINE]);
        s.set_line(MAX_LINE, false);
        let grown = s.buffer.capacity();
        assert!(grown >= MAX_LINE);

        // The line goes away; the capacity must not.
        s.buffer.clear();
        s.buffer.extend_from_slice(b"ls");
        s.set_line(2, false);
        assert_eq!(s.buffer.len(), 2);
        assert!(
            s.buffer.capacity() < grown,
            "the scratch must give back the capacity of a line it no longer holds"
        );
        assert!(
            s.buffer.capacity() >= s.buffer.len(),
            "the scratch must keep enough room for the line it holds"
        );
    }

    #[test]
    fn the_scratch_does_not_reallocate_for_every_keystroke() {
        // The shrink must be rare: a reallocation on the interactive thread
        // is exactly what the scratch exists to avoid.
        let mut s = Scratch::new();
        s.buffer.clear();
        s.buffer.extend_from_slice(&vec![b'x'; 512]);
        s.set_line(512, false);
        let capacity = s.buffer.capacity();
        for _ in 0..100 {
            s.buffer.clear();
            s.buffer.extend_from_slice(b"echo hi");
            s.set_line(7, false);
        }
        assert_eq!(
            s.buffer.capacity(),
            capacity,
            "a short line must not make the scratch reallocate"
        );
    }

    #[test]
    fn the_scratch_keeps_the_capacity_a_line_needs() {
        let mut s = Scratch::new();
        s.buffer.extend_from_slice(&vec![b'x'; 2048]);
        s.set_line(2048, false);
        let capacity = s.buffer.capacity();
        // The same line again: nothing changed, so nothing may be
        // reallocated.
        s.set_line(2048, false);
        assert_eq!(s.buffer.capacity(), capacity);
        // A line that is still within the retained capacity is fine too.
        s.buffer.clear();
        s.buffer.extend_from_slice(&vec![b'x'; 1024]);
        s.set_line(1024, false);
        assert_eq!(
            s.buffer.capacity(),
            capacity,
            "a line that fits in the retained capacity must not reallocate"
        );
    }

    // --- the command words --------------------------------------------------

    /// The tokens of a line, as the lexer sees them.
    fn tokens_of(line: &str) -> Vec<Token> {
        lexer::lex(line.as_bytes())
            .expect("a scannable line")
            .tokens
    }

    /// Run `f` with a C argument vector, the way zsh passes one to a builtin.
    ///
    /// The strings stay alive for the duration of the call, so the pointers
    /// handed to `f` are valid — which is what the module relies on when
    /// it copies the words out of them.
    fn with_argv<R>(args: &[&[u8]], f: impl FnOnce(*mut *mut std::ffi::c_char) -> R) -> R {
        let strings: Vec<std::ffi::CString> = args
            .iter()
            .map(|arg| std::ffi::CString::new(*arg).expect("an argument without a NUL"))
            .collect();
        let mut pointers: Vec<*mut std::ffi::c_char> = strings
            .iter()
            .map(|s| s.as_ptr() as *mut std::ffi::c_char)
            .collect();
        pointers.push(std::ptr::null_mut());
        f(pointers.as_mut_ptr())
    }

    /// The answers the module reads out of this argument vector.
    fn answers_from(args: &[&[u8]]) -> CommandAnswers {
        let mut answers = CommandAnswers::new();
        with_argv(args, |argv| answers.set_from_args(argv));
        answers
    }

    /// The spans of a line, with no path lookups.
    ///
    /// The path lookup is the only part of the highlighter that needs a
    /// shell, so it is left out here: what is under test is what the
    /// *command* words look like.
    fn spans_of(line: &str, answers: &CommandAnswers) -> Vec<HighlightSpan> {
        let tokens = tokens_of(line);
        let mut spans = Vec::new();
        build_spans_with(
            &mut spans,
            line.as_bytes(),
            &tokens,
            answers,
            &mut |_word| None,
        );
        spans
    }

    #[test]
    fn a_command_the_shell_knows_is_the_command_style() {
        let line = b"ls";
        let word = &tokens_of("ls")[..1];
        let answers = answers_from(&[b"--command-known=ls"]);
        assert_eq!(
            command_style(word, line, &answers, &mut |_word| None),
            WordStyle::Styled(StyleId::Command),
            "a command the shell knows is green"
        );
    }

    #[test]
    fn a_command_the_shell_does_not_know_is_the_unknown_style() {
        let line = b"nope";
        let word = &tokens_of("nope")[..1];
        let answers = answers_from(&[b"--command-unknown=nope"]);
        assert_eq!(
            command_style(word, line, &answers, &mut |_word| None),
            WordStyle::Styled(StyleId::UnknownCommand),
            "a command the shell does not know is red"
        );
    }

    #[test]
    fn a_command_the_shell_said_nothing_about_is_left_alone() {
        // The lexer's own style for a command word is green, and green
        // asserts that the shell knows the command — which was never
        // established. So a command word with no answer gets no region at
        // all, and stays exactly as the user typed it.
        let line = b"ls";
        let word = &tokens_of("ls")[..1];
        let answers = answers_from(&[]);
        assert_eq!(
            command_style(word, line, &answers, &mut |_word| None),
            WordStyle::Unstyled,
            "a command nobody said anything about must not be styled"
        );
    }

    #[test]
    fn an_answer_about_another_word_is_dropped() {
        // The shell and the lexer do not always agree on where the command
        // word is. When they do not, the answer must not be applied to the
        // word the lexer found: colouring the wrong word red would be
        // worse than not colouring it at all.
        let line = b"ls";
        let word = &tokens_of("ls")[..1];
        let answers = answers_from(&[b"--command-unknown=other"]);
        assert_eq!(
            command_style(word, line, &answers, &mut |_word| None),
            WordStyle::Unstyled,
            "an answer about another word must be dropped"
        );
    }

    #[test]
    fn a_command_word_that_starts_with_a_tilde_is_a_command_word() {
        // The scanner emits a leading `~` as a glob, because that is what
        // it is in `~/*.txt`, and the rest of the word keeps the kind the
        // word has. `~/bin/tool` is therefore a glob followed by a command
        // — and it is a command word, so the shell's answer applies to it.
        let line = b"~/bin/tool";
        let tokens = tokens_of("~/bin/tool");
        let word = &tokens[..];
        assert!(
            is_command_word(word, line),
            "a word that starts with a tilde and holds a command is a command word"
        );
        let answers = answers_from(&[b"--command-known=~/bin/tool"]);
        assert_eq!(
            command_style(word, line, &answers, &mut |_word| None),
            WordStyle::Styled(StyleId::Command),
            "the answer must apply to the whole word"
        );
    }

    #[test]
    fn a_command_inside_a_substitution_is_not_a_command_word() {
        // The `cmd` of `$(cmd)` is a command, but the word `$(cmd)` is a
        // substitution: the word is not a command word, and the command
        // inside it is styled on its own.
        let line = b"$(cmd)";
        let tokens = tokens_of("$(cmd)");
        let word = &tokens[..];
        assert!(
            !is_command_word(word, line),
            "a word that holds a command inside a substitution is not a command word"
        );
    }

    #[test]
    fn a_command_word_that_names_a_file_is_a_path_not_an_unknown_command() {
        // `~/bin/tool` is a path, not a typo. The reference implementation
        // looks the word up as a command first and only then checks
        // whether it is a path
        // (`research/upstream/fsh/fast-highlight:1265-1295`).
        let line = b"~/bin/tool";
        let tokens = tokens_of("~/bin/tool");
        let answers = answers_from(&[b"--command-unknown=~/bin/tool"]);
        assert_eq!(
            command_style(&tokens[..], line, &answers, &mut |_word| Some(
                StyleId::PathToDir
            ),),
            WordStyle::Styled(StyleId::PathToDir),
            "a command word that names a file is a path"
        );
    }

    #[test]
    fn the_published_spans_are_the_ones_the_module_publishes() {
        // The function the benchmark and the tests use must publish what
        // the module publishes: the command word green when the shell
        // knows it, red when it does not, and nothing at all when the
        // shell was not asked.
        assert_eq!(
            published_spans(b"ls", &[b"--command-known=ls"]),
            vec![HighlightSpan::new(0, 2, StyleId::Command)],
            "a command the shell knows must be published as a command"
        );
        assert_eq!(
            published_spans(b"ls", &[b"--command-unknown=ls"]),
            vec![HighlightSpan::new(0, 2, StyleId::UnknownCommand)],
            "a command the shell does not know must be published as unknown"
        );
        assert!(
            published_spans(b"ls", &[]).is_empty(),
            "a command word nobody said anything about must get no span"
        );
        assert!(
            published_spans(b"ls", &[b"--command-known=other"]).is_empty(),
            "an answer about another word must not be applied"
        );
    }

    #[test]
    fn every_command_word_gets_its_own_answer() {
        // The answers are a list, and each one is about its own word.
        let spans = published_spans(
            b"ls | grep x",
            &[b"--command-known=ls", b"--command-known=grep"],
        );
        assert_eq!(
            spans,
            vec![
                HighlightSpan::new(0, 2, StyleId::Command),
                HighlightSpan::new(3, 4, StyleId::Operator),
                HighlightSpan::new(5, 9, StyleId::Command),
                HighlightSpan::new(10, 11, StyleId::Argument),
            ],
            "both command words must be published, and the operator and the \
             argument must be published as what they are"
        );
    }

    #[test]
    fn the_answers_are_read_out_of_the_argument_vector() {
        let answers = answers_from(&[
            b"--command-known=ls",
            b"--command-unknown=nosuchcmd",
            b"--region-highlight",
        ]);
        assert_eq!(answers.knowledge_of(b"ls"), Some(CommandKnowledge::Known));
        assert_eq!(
            answers.knowledge_of(b"nosuchcmd"),
            Some(CommandKnowledge::Unknown)
        );
        assert_eq!(
            answers.knowledge_of(b"somethingelse"),
            None,
            "a word the shell said nothing about has no answer"
        );
    }

    #[test]
    fn the_old_answer_protocol_is_not_honoured() {
        // The protocol used to be `--command-word=<word>` plus a bare
        // `--command-known` or `--command-unknown`. Both are gone: an
        // argument that looks like the old protocol must not be read as an
        // answer, or a stale adapter would paint every command word red.
        let answers = answers_from(&[b"--command-word=ls", b"--command-known"]);
        assert_eq!(
            answers.knowledge_of(b"ls"),
            None,
            "the old protocol must not be honoured"
        );
    }

    #[test]
    fn a_command_the_shell_knows_is_published_as_green() {
        let answers = answers_from(&[b"--command-known=ls"]);
        assert_eq!(
            spans_of("ls", &answers),
            vec![HighlightSpan::new(0, 2, StyleId::Command)],
            "a command the shell knows must be published as a command"
        );
    }

    #[test]
    fn a_command_the_shell_does_not_know_is_published_as_red() {
        let answers = answers_from(&[b"--command-unknown=nosuchcmd"]);
        assert_eq!(
            spans_of("nosuchcmd", &answers),
            vec![HighlightSpan::new(0, 9, StyleId::UnknownCommand)],
            "a command the shell does not know must be published as unknown"
        );
    }

    #[test]
    fn a_command_word_with_no_answer_is_not_published_at_all() {
        // The bug this fixes: `ls | nosuchcmd` painted the second command
        // word green, because the lexer's own style for a command word is
        // green and the module fell back to it.
        let answers = answers_from(&[]);
        assert!(
            spans_of("ls", &answers).is_empty(),
            "a command nobody said anything about must not be published"
        );
        let spans = spans_of("ls | nosuchcmd", &answers);
        assert!(
            !spans.iter().any(|s| s.style == StyleId::Command),
            "no command may be published without an answer: {spans:?}"
        );
        assert!(
            !spans.iter().any(|s| s.style == StyleId::UnknownCommand),
            "no command may be published without an answer: {spans:?}"
        );
    }

    #[test]
    fn several_answers_are_kept_apart() {
        // A line can hold more than one command word, and the shell
        // answers for each of them. The right answer must be applied to
        // the right word.
        let answers = answers_from(&[b"--command-known=ls", b"--command-unknown=nosuchcmd"]);
        let spans = spans_of("ls | nosuchcmd", &answers);
        assert_eq!(
            spans,
            vec![
                HighlightSpan::new(0, 2, StyleId::Command),
                HighlightSpan::new(3, 4, StyleId::Operator),
                HighlightSpan::new(5, 14, StyleId::UnknownCommand),
            ],
            "each command word must get its own answer"
        );
    }

    #[test]
    fn an_argument_is_not_published_as_a_command() {
        // Only the command word is a command. `ls` here is an argument, and
        // it must not be coloured like one — even when the shell knows the
        // word `ls`.
        let answers = answers_from(&[b"--command-known=echo"]);
        let spans = spans_of("echo ls", &answers);
        assert_eq!(
            spans,
            vec![
                HighlightSpan::new(0, 4, StyleId::Command),
                HighlightSpan::new(5, 7, StyleId::Argument),
            ],
            "only the command word may be a command"
        );
        assert!(
            !spans[1..]
                .iter()
                .any(|s| matches!(s.style, StyleId::Command | StyleId::UnknownCommand)),
            "an argument must never be published as a command"
        );
    }

    #[test]
    fn a_command_inside_a_substitution_is_not_published_without_an_answer() {
        // The command of a command substitution is a command in its own
        // right, and the lexer's green for it says the same thing the
        // green of a command word says: that the shell knows it.
        let answers = answers_from(&[]);
        let spans = spans_of("echo $(cmd)", &answers);
        assert!(
            !spans.iter().any(|s| s.style == StyleId::Command),
            "no command may be published without an answer: {spans:?}"
        );
        let answers = answers_from(&[b"--command-known=cmd"]);
        let spans = spans_of("echo $(cmd)", &answers);
        assert!(
            spans.iter().any(|s| s.style == StyleId::Command),
            "a command the shell knows is published: {spans:?}"
        );
    }

    // --- words --------------------------------------------------------------

    #[test]
    fn a_word_is_the_bytes_its_tokens_cover() {
        let line = b"echo 'hi there'";
        let tokens = tokens_of("echo 'hi there'");
        let word = &tokens[1..2];
        assert_eq!(word_text(word, line), b"'hi there'");
    }

    #[test]
    fn only_a_literal_word_can_name_a_file() {
        let line = b"cat /etc/passwd";
        let tokens = tokens_of("cat /etc/passwd");
        assert!(
            is_literal(&tokens[1..2], line),
            "a plain word is literal text"
        );
    }

    #[test]
    fn a_quoted_word_is_not_literal() {
        // The file it names is not knowable without running the shell.
        let line = b"cat \"/etc/passwd\"";
        let tokens = tokens_of("cat \"/etc/passwd\"");
        let quoted: Vec<Token> = tokens[1..].to_vec();
        assert!(!is_literal(&quoted, line), "a quoted word is not literal");
    }

    #[test]
    fn a_glob_is_not_literal() {
        let line = b"cat *.txt";
        let tokens = tokens_of("cat *.txt");
        let glob: Vec<Token> = tokens[1..].to_vec();
        assert!(!is_literal(&glob, line), "a glob is not literal text");
    }

    #[test]
    fn a_tilde_is_literal_text() {
        // `~` is a tilde expansion rather than a glob, and the rest of the
        // word is still literal text.
        let line = b"cat ~/notes";
        let tokens = tokens_of("cat ~/notes");
        let word: Vec<Token> = tokens[1..].to_vec();
        assert!(is_literal(&word, line), "a tilde word is literal text");
        assert_eq!(word_text(&word, line), b"~/notes");
    }

    // --- the diff -----------------------------------------------------------

    #[test]
    fn the_diff_agrees_with_the_highlighter_crates() {
        // The module publishes spans that are not the lexer's, so it has
        // its own diff. It must agree with the one in the `highlight`
        // crate, which is the specification.
        use highlight::SpanSet;
        let lines = ["echo hi", "echo hi there", "git commit --amend", ""];
        for a in lines {
            for b in lines {
                let old = SpanSet::from_tokens(&tokens_of(a));
                let new = SpanSet::from_tokens(&tokens_of(b));
                let old_spans: Vec<HighlightSpan> = old.spans().to_vec();
                let new_spans: Vec<HighlightSpan> = new.spans().to_vec();
                let mine = changed_ranges(&old_spans, &new_spans);
                let theirs = highlight::diff(&old, &new);
                assert_eq!(mine.len(), theirs.len(), "for {a:?} vs {b:?}");
                for (m, t) in mine.iter().zip(theirs.iter()) {
                    assert_eq!((m.start, m.end), (t.start, t.end), "for {a:?} vs {b:?}");
                }
            }
        }
    }

    // --- the path cache -----------------------------------------------------

    #[test]
    fn the_path_cache_answers_the_same_twice() {
        let mut cache = PathCache::default();
        let now = Instant::now();
        let dir = std::env::temp_dir();
        let first = cache.lookup(dir.to_string_lossy().as_bytes(), now);
        assert_eq!(
            first,
            Some(PathKind::Dir),
            "the temporary directory is a directory"
        );
        // The second answer comes from the cache, and must be the same.
        assert_eq!(
            cache.lookup(dir.to_string_lossy().as_bytes(), now),
            first,
            "the second answer must come from the cache"
        );
    }

    #[test]
    fn the_path_cache_remembers_a_path_that_does_not_exist() {
        let mut cache = PathCache::default();
        let now = Instant::now();
        let missing = b"/no/such/path/for/the/test";
        assert_eq!(cache.lookup(missing, now), None);
        assert_eq!(cache.lookup(missing, now), None, "the answer is remembered");
    }

    /// A scratch directory that belongs to one test alone.
    ///
    /// The name carries the process id and the test's own name: cargo runs
    /// the tests of a binary in parallel, and two tests that shared one
    /// directory would delete each other's files — a failure that would
    /// appear only when the machine was busy.
    fn scratch_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("zsh-pulse-path-ttl-{}-{label}", std::process::id()))
    }

    #[test]
    fn a_fresh_answer_is_served_from_the_cache() {
        // The answer is only trusted for the TTL: within it, the path is
        // not looked up again — even though the file has gone.
        let mut cache = PathCache::default();
        let now = Instant::now();
        let dir = scratch_dir("fresh");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the test directory");
        let file = dir.join("file");
        std::fs::write(&file, b"x").expect("the test file");
        let path = file.to_string_lossy().into_owned();

        assert_eq!(cache.lookup(path.as_bytes(), now), Some(PathKind::File));

        // The file is gone, but the answer is still fresh.
        std::fs::remove_file(&file).expect("the test file");
        assert_eq!(
            cache.lookup(path.as_bytes(), now + PATH_TTL / 2),
            Some(PathKind::File),
            "a fresh answer must be served from the cache"
        );
    }

    #[test]
    fn an_expired_answer_is_looked_up_again() {
        // A file that is deleted while the user is typing must stop being
        // highlighted, and a file that is created must start being
        // highlighted. Both need the answer to expire.
        let mut cache = PathCache::default();
        let now = Instant::now();
        let dir = scratch_dir("expired");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the test directory");
        let file = dir.join("file");
        let path = file.to_string_lossy().into_owned();

        // The file does not exist yet.
        assert_eq!(cache.lookup(path.as_bytes(), now), None);

        // It is created, and the answer has not expired: the cache is
        // still trusted, so the file is not picked up yet.
        std::fs::write(&file, b"x").expect("the test file");
        assert_eq!(
            cache.lookup(path.as_bytes(), now + PATH_TTL / 2),
            None,
            "a fresh answer must be served from the cache"
        );

        // The answer has expired: the path is looked up again, and the
        // file is there.
        let later = now + PATH_TTL + Duration::from_millis(1);
        assert_eq!(
            cache.lookup(path.as_bytes(), later),
            Some(PathKind::File),
            "an expired answer must be looked up again"
        );

        // And the other way round: a file that is deleted stops being
        // highlighted once the answer has expired.
        std::fs::remove_file(&file).expect("the test file");
        assert_eq!(
            cache.lookup(path.as_bytes(), later + PATH_TTL + Duration::from_millis(1)),
            None,
            "an expired answer must be looked up again"
        );
    }

    #[test]
    fn the_path_cache_does_not_grow_without_bound() {
        let mut cache = PathCache::default();
        let now = Instant::now();
        for i in 0..(MAX_CACHED_PATHS * 2) {
            let path = format!("/no/such/path/{i}");
            cache.lookup(path.as_bytes(), now);
        }
        assert!(
            cache.entries.len() <= MAX_CACHED_PATHS,
            "the cache must stay bounded: {} entries",
            cache.entries.len()
        );
    }
}

//! The precommands of zsh, and the words that follow them.
//!
//! A precommand is a command that runs another command: `sudo true` runs
//! `true`, `env FOO=bar cmd` runs `cmd`, and `command -v ls` asks about
//! `ls`. The command word after the precommand is a **command**, not an
//! argument, and it is highlighted as one.
//!
//! The lexer cannot say that: it marks the first word of a command and
//! stops. The precommand is the first word, and the command it runs is the
//! second. The answer has to come from the grammar of the precommand
//! itself, and that is what this module holds.
//!
//! The set of precommands and the options that take an argument follow
//! zsh-patina's default syntax definition
//! (`assets/Packages/ShellScript/Bash.sublime-syntax` in the reference
//! tree): `-`, `builtin`, `noglob`, `command`, `exec`, `doas`, `env`,
//! `nice`, `nohup`, `sudo`, `sudoedit`, plus the `time` and `nocorrect`
//! keywords.
//!
//! The parser here is deliberately small. It knows which options take an
//! argument, because treating an option's argument as the command would
//! colour the wrong word; it does not know the full option table of any
//! command, and it does not need to. Everything it cannot parse is left as
//! an ordinary argument, which is the safe direction: a missing colour is
//! better than a wrong one.

/// A command that runs another command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `-`: run the command with the default options.
    Dash,
    /// `builtin`: run a builtin.
    Builtin,
    /// `noglob`: disable globbing for the command.
    Noglob,
    /// `command`: run a command, bypassing functions and aliases.
    Command,
    /// `exec`: replace the shell with a command.
    Exec,
    /// `doas`: run a command as another user.
    Doas,
    /// `env`: run a command in a modified environment.
    Env,
    /// `nice`: run a command with a different scheduling priority.
    Nice,
    /// `nohup`: run a command that ignores `SIGHUP`.
    Nohup,
    /// `sudo`: run a command as another user.
    Sudo,
    /// `sudoedit`: edit files as another user.
    SudoEdit,
    /// `time`: time the command that follows.
    Time,
    /// `nocorrect`: disable correction for the command that follows.
    Nocorrect,
}

impl Kind {
    /// The precommand this word names, if it is one.
    ///
    /// The word is the raw text of the word as it appears on the line, the
    /// same text the shell was asked about. A quoted precommand is not
    /// recognised: `'sudo' ls` runs a command whose name is `sudo`, but the
    /// quotes are part of the word the user sees, and the grammar of the
    /// line is no longer the plain precommand form.
    pub(crate) fn of(word: &[u8]) -> Option<Self> {
        Some(match word {
            b"-" => Self::Dash,
            b"builtin" => Self::Builtin,
            b"noglob" => Self::Noglob,
            b"command" => Self::Command,
            b"exec" => Self::Exec,
            b"doas" => Self::Doas,
            b"env" => Self::Env,
            b"nice" => Self::Nice,
            b"nohup" => Self::Nohup,
            b"sudo" => Self::Sudo,
            b"sudoedit" => Self::SudoEdit,
            b"time" => Self::Time,
            b"nocorrect" => Self::Nocorrect,
            _ => return None,
        })
    }

    /// Whether this precommand's options have a value.
    fn short_option_takes_argument(self, option: u8) -> bool {
        match self {
            // `sudo -u user`, `-g group`, `-p prompt`, ...
            Self::Sudo | Self::SudoEdit => matches!(
                option,
                b'C' | b'D' | b'g' | b'p' | b'R' | b'r' | b'T' | b't' | b'U' | b'u'
            ),
            // `doas -u user`, `-a style`, `-C config`.
            Self::Doas => matches!(option, b'a' | b'C' | b'u'),
            // `env -u name`, `-C dir`, `-P path`, `-S string`.
            Self::Env => matches!(option, b'C' | b'P' | b'S' | b'u'),
            // `nice -n adjustment`.
            Self::Nice => option == b'n',
            // `exec -a argv0`.
            Self::Exec => option == b'a',
            _ => false,
        }
    }

    /// Whether this long option of this precommand has a value.
    fn long_option_takes_argument(self, option: &[u8]) -> bool {
        match self {
            Self::Sudo | Self::SudoEdit => matches!(
                option,
                b"chdir"
                    | b"chroot"
                    | b"close-from"
                    | b"command-timeout"
                    | b"group"
                    | b"host"
                    | b"other-user"
                    | b"preserve-env"
                    | b"prompt"
                    | b"user"
            ),
            Self::Env => matches!(option, b"argv0" | b"chdir" | b"split-string" | b"unset"),
            Self::Nice => option == b"adjustment",
            _ => false,
        }
    }
}

/// What one word of a precommand segment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// An option; it keeps the style the lexer gave it.
    Option,
    /// The value of an option; it is an argument, not a command.
    OptionArgument,
    /// A `NAME=value` assignment; it is not a command.
    Assignment,
    /// The command the precommand runs.
    Command,
    /// A command name in the list after `command -v` or `-V`.
    CommandName,
    /// An ordinary operand that is not a command (the files of
    /// `sudoedit` / `sudo -e`).
    Operand,
}

/// The parse state of one precommand segment.
///
/// A segment starts at the precommand and ends at the next command
/// separator. The state is small and lives on the stack: no allocation is
/// needed to highlight a precommand line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct State {
    kind: Kind,
    /// `--` has been seen: the remaining words are operands.
    options_done: bool,
    /// The next word is the value of an option.
    option_argument: bool,
    /// `command -v` or `command -V` has been seen: every following operand
    /// names a command.
    command_names: bool,
    /// `sudo -e` or `sudo --edit` has been seen, or the precommand is
    /// `sudoedit`: the remaining operands are files, not a command.
    edit: bool,
    /// The command the precommand runs has been found.
    done: bool,
}

impl State {
    /// Start parsing the words that follow `kind`.
    pub(crate) fn new(kind: Kind) -> Self {
        Self {
            kind,
            options_done: false,
            option_argument: false,
            command_names: false,
            edit: kind == Kind::SudoEdit,
            done: false,
        }
    }

    /// Whether the command word has been found.
    ///
    /// The caller drops the state when this is true: the words that follow
    /// are ordinary arguments.
    pub(crate) fn is_done(self) -> bool {
        self.done
    }

    /// What this word is, and what it means for the words after it.
    ///
    /// `lexed_as_option` says the lexer classified the word as an option.
    /// A quoted `"-n"` is not one: the shell would pass it as a literal
    /// argument, and the lexer's class is what tells the two apart.
    pub(crate) fn classify(&mut self, word: &[u8], lexed_as_option: bool) -> Role {
        // A word after `--` is never an option. That is the whole point of
        // the marker, and it is checked before the lexer's answer because
        // the lexer's state and this one need not agree on where the
        // precommand's options begin.
        if !self.options_done && word == b"--" {
            self.options_done = true;
            return Role::Option;
        }

        if self.option_argument {
            self.option_argument = false;
            return Role::OptionArgument;
        }

        if !self.options_done && lexed_as_option {
            self.parse_option(word);
            return Role::Option;
        }

        if self.command_names {
            // After `command -v`/`-V`, every operand is a command name.
            // The list continues to the end of the simple command.
            return Role::CommandName;
        }

        if self.edit {
            // `sudo -e file1 file2` and `sudoedit file1 file2` take files,
            // not a command. No word here may be coloured as one.
            return Role::Operand;
        }

        if is_assignment(word) {
            // `env FOO=bar cmd` sets a variable and then runs `cmd`.
            return Role::Assignment;
        }

        // The command the precommand runs. Everything after it is an
        // ordinary argument.
        self.done = true;
        Role::Command
    }

    /// Record the options of one option word.
    fn parse_option(&mut self, word: &[u8]) {
        if word.starts_with(b"--") {
            self.parse_long_option(&word[2..]);
        } else if word.len() > 1 {
            self.parse_short_options(&word[1..]);
        }
    }

    /// Record a short option, or a bundle of them.
    ///
    /// The last character of a bundle may be an option whose value is the
    /// next word (`-u root`); a character followed by more text has its
    /// value attached (`-uroot`, `-n5`). An option that takes a value
    /// therefore ends the bundle either way.
    fn parse_short_options(&mut self, options: &[u8]) {
        for (index, &option) in options.iter().enumerate() {
            // `sudo -e` switches to edit mode. The character may be part
            // of a bundle, as in `-ie`.
            if matches!(self.kind, Kind::Sudo | Kind::SudoEdit) && option == b'e' {
                self.edit = true;
            }
            // `command -v` and `-V` turn every following operand into a
            // command name. `-p` is an ordinary option.
            if self.kind == Kind::Command && matches!(option, b'v' | b'V') {
                self.command_names = true;
            }
            if self.kind.short_option_takes_argument(option) {
                if index + 1 == options.len() {
                    self.option_argument = true;
                }
                // The rest of the bundle, if there is any, is the value.
                return;
            }
        }
    }

    /// Record a long option.
    ///
    /// `--user=root` carries its value in the same word; `--user root`
    /// takes the next one. Only the names that are known to take a value
    /// are consumed, which is the safe direction: an unknown option never
    /// swallows the command word.
    fn parse_long_option(&mut self, option: &[u8]) {
        let (name, has_value) = match option.iter().position(|&b| b == b'=') {
            Some(at) => (&option[..at], true),
            None => (option, false),
        };

        if matches!(self.kind, Kind::Sudo | Kind::SudoEdit) && name == b"edit" && !has_value {
            self.edit = true;
            return;
        }

        if !has_value && self.kind.long_option_takes_argument(name) {
            self.option_argument = true;
        }
    }
}

/// Whether the word is a `NAME=value` assignment.
///
/// The lexer only calls an assignment an assignment in command position;
/// after a precommand the shell still reads `FOO=bar` as one (`env
/// FOO=bar cmd`), so the shape is checked here. It is the same shape the
/// lexer uses (`crates/lexer/src/scanner.rs`, `looks_like_assignment`).
fn is_assignment(word: &[u8]) -> bool {
    let Some(&first) = word.first() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    let mut i = 1;
    while word
        .get(i)
        .is_some_and(|&b| b.is_ascii_alphanumeric() || b == b'_')
    {
        i += 1;
    }
    match word.get(i) {
        Some(b'=') => true,
        Some(b'+') => word.get(i + 1) == Some(&b'='),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The roles of the words after `precommand`, with `sudo`-style option
    /// classification taken from the text itself.
    fn roles(kind: Kind, words: &[&str]) -> Vec<Role> {
        let mut state = State::new(kind);
        words
            .iter()
            .map(|word| {
                let word = word.as_bytes();
                let looks_like_option = word.starts_with(b"-") && word.len() > 1;
                state.classify(word, looks_like_option)
            })
            .collect()
    }

    #[test]
    fn every_reference_precommand_is_recognised() {
        for word in [
            "-",
            "builtin",
            "noglob",
            "command",
            "exec",
            "doas",
            "env",
            "nice",
            "nohup",
            "sudo",
            "sudoedit",
            "time",
            "nocorrect",
        ] {
            assert!(
                Kind::of(word.as_bytes()).is_some(),
                "{word} is a precommand in the reference grammar"
            );
        }
        assert_eq!(Kind::of(b"sudo ls"), None, "a precommand is one word");
        assert_eq!(
            Kind::of(b"'sudo'"),
            None,
            "a quoted word is not a precommand"
        );
    }

    #[test]
    fn the_command_after_a_precommand_is_a_command() {
        assert_eq!(roles(Kind::Sudo, &["ls"]), vec![Role::Command]);
        assert_eq!(roles(Kind::Nohup, &["ls"]), vec![Role::Command]);
        assert_eq!(roles(Kind::Builtin, &["echo"]), vec![Role::Command]);
        assert_eq!(roles(Kind::Noglob, &["cat"]), vec![Role::Command]);
        assert_eq!(roles(Kind::Dash, &["ls"]), vec![Role::Command]);
        assert_eq!(roles(Kind::Time, &["sleep"]), vec![Role::Command]);
        assert_eq!(roles(Kind::Nocorrect, &["slep"]), vec![Role::Command]);
    }

    #[test]
    fn the_options_of_sudo_are_consumed() {
        assert_eq!(
            roles(Kind::Sudo, &["-n", "ls"]),
            vec![Role::Option, Role::Command]
        );
        assert_eq!(
            roles(Kind::Sudo, &["-n", "-u", "root", "ls"]),
            vec![
                Role::Option,
                Role::Option,
                Role::OptionArgument,
                Role::Command
            ]
        );
        assert_eq!(
            roles(Kind::Sudo, &["-uroot", "ls"]),
            vec![Role::Option, Role::Command],
            "an attached value must not become the command"
        );
        assert_eq!(
            roles(Kind::Sudo, &["--user=root", "ls"]),
            vec![Role::Option, Role::Command]
        );
        assert_eq!(
            roles(Kind::Sudo, &["--user", "root", "ls"]),
            vec![Role::Option, Role::OptionArgument, Role::Command]
        );
        assert_eq!(
            roles(Kind::Sudo, &["--", "-n"]),
            vec![Role::Option, Role::Command],
            "the command after -- may look like an option"
        );
    }

    #[test]
    fn an_option_value_that_looks_like_an_option_is_still_a_value() {
        // `nice -n -35 date`: `-35` is the adjustment, not an option and
        // not the command.
        assert_eq!(
            roles(Kind::Nice, &["-n", "-35", "date"]),
            vec![Role::Option, Role::OptionArgument, Role::Command]
        );
        assert_eq!(
            roles(Kind::Nice, &["-n5", "date"]),
            vec![Role::Option, Role::Command]
        );
        assert_eq!(
            roles(Kind::Nice, &["--adjustment", "5", "date"]),
            vec![Role::Option, Role::OptionArgument, Role::Command]
        );
    }

    #[test]
    fn env_assignments_are_not_the_command() {
        assert_eq!(
            roles(Kind::Env, &["-i", "FOO=bar", "cmd"]),
            vec![Role::Option, Role::Assignment, Role::Command]
        );
        assert_eq!(
            roles(Kind::Env, &["-u", "NAME", "cmd"]),
            vec![Role::Option, Role::OptionArgument, Role::Command]
        );
        assert_eq!(
            roles(Kind::Env, &["--unset=NAME", "cmd"]),
            vec![Role::Option, Role::Command]
        );
        assert_eq!(
            roles(Kind::Env, &["FOO+=bar", "cmd"]),
            vec![Role::Assignment, Role::Command]
        );
    }

    #[test]
    fn command_v_takes_a_list_of_command_names() {
        assert_eq!(
            roles(Kind::Command, &["-v", "echo", "file"]),
            vec![Role::Option, Role::CommandName, Role::CommandName]
        );
        assert_eq!(
            roles(Kind::Command, &["-V", "echo"]),
            vec![Role::Option, Role::CommandName]
        );
        assert_eq!(
            roles(Kind::Command, &["-pv", "echo", "file"]),
            vec![Role::Option, Role::CommandName, Role::CommandName],
            "an option bundle may carry the switch"
        );
        assert_eq!(
            roles(Kind::Command, &["-p", "echo"]),
            vec![Role::Option, Role::Command],
            "without -v there is one command, as usual"
        );
    }

    #[test]
    fn sudo_edit_mode_has_no_command() {
        assert_eq!(
            roles(Kind::Sudo, &["-e", "file1", "file2"]),
            vec![Role::Option, Role::Operand, Role::Operand]
        );
        assert_eq!(
            roles(Kind::Sudo, &["--edit", "file1", "file2"]),
            vec![Role::Option, Role::Operand, Role::Operand]
        );
        assert_eq!(
            roles(Kind::SudoEdit, &["-u", "user", "file1"]),
            vec![Role::Option, Role::OptionArgument, Role::Operand]
        );
    }

    #[test]
    fn the_last_command_of_a_chain_is_the_one_that_runs() {
        // The caller starts a new state when the command it found is itself
        // a precommand. The words of the inner precommand are then parsed
        // by the inner state: `env` sees `FOO=bar` as an assignment and
        // `ls` as its command.
        let mut outer = State::new(Kind::Sudo);
        assert_eq!(outer.classify(b"-u", true), Role::Option);
        assert_eq!(outer.classify(b"root", false), Role::OptionArgument);
        assert_eq!(outer.classify(b"env", false), Role::Command);
        assert!(outer.is_done());

        let mut inner = State::new(Kind::Env);
        assert_eq!(inner.classify(b"FOO=bar", false), Role::Assignment);
        assert_eq!(inner.classify(b"ls", false), Role::Command);
    }

    #[test]
    fn an_unknown_option_never_swallows_the_command() {
        // The safe direction: an option this parser does not know is
        // treated as an option without a value, so the next word is still
        // the command.
        assert_eq!(
            roles(Kind::Nohup, &["--version", "ls"]),
            vec![Role::Option, Role::Command]
        );
        assert_eq!(
            roles(Kind::Exec, &["-c", "ls"]),
            vec![Role::Option, Role::Command]
        );
    }
}

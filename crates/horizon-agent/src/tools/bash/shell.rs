//! Shared shell-word recognition for proactive Git and Cargo classifiers.
//! This lexer proposes UX decisions; containment and approval own authority.

/// Shell separators retained by the lexer for command classifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Separator {
    And,
    Or,
    Semicolon,
    Newline,
    Pipe,
    Background,
    OpenParen,
    CloseParen,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ShellToken {
    Word(String),
    Separator(Separator),
}

/// Tests each command segment in order, including the trailing segment.
/// Git and Cargo share separator handling while keeping their own predicates.
pub(super) fn any_command_segment(
    command: &str,
    mut matches: impl FnMut(&[String]) -> bool,
) -> bool {
    let mut segment = Vec::new();
    for token in tokenize(command) {
        match token {
            ShellToken::Word(word) => segment.push(word),
            ShellToken::Separator(_) => {
                if matches(&segment) {
                    return true;
                }
                segment.clear();
            }
        }
    }
    matches(&segment)
}

/// Finds a directly invoked command after the small set of shell prefixes
/// understood by Horizon's proactive command classifiers. This is not a
/// security parser: unsupported shell syntax falls through to containment.
pub(super) fn executable_index(words: &[String]) -> Option<usize> {
    let mut index = 0;
    while words.get(index).is_some_and(|word| is_assignment(word)) {
        index += 1;
    }
    loop {
        match words.get(index).map(String::as_str) {
            Some("command") => {
                index += 1;
                while let Some(option) = words.get(index).map(String::as_str) {
                    match option {
                        "-p" => index += 1,
                        "--" => {
                            index += 1;
                            break;
                        }
                        "-v" | "-V" => return None,
                        value if value.starts_with('-') => return None,
                        _ => break,
                    }
                }
            }
            Some("env") => {
                index += 1;
                while let Some(word) = words.get(index).map(String::as_str) {
                    match word {
                        value if is_assignment(value) => index += 1,
                        "-u" | "--unset" | "-C" | "--chdir" | "-S" | "--split-string" => {
                            index += 2;
                        }
                        "--" => {
                            index += 1;
                            break;
                        }
                        value
                            if value.starts_with("--unset=")
                                || value.starts_with("--chdir=")
                                || value.starts_with("--split-string=")
                                || value.starts_with("--argv0=") =>
                        {
                            index += 1;
                        }
                        value if value.starts_with('-') => index += 1,
                        _ => break,
                    }
                }
            }
            Some("nice") => {
                index += 1;
                if words.get(index).map(String::as_str) == Some("-n") {
                    index += 2;
                }
            }
            Some("timeout") => {
                index += 1;
                while words.get(index).is_some_and(|word| word.starts_with('-')) {
                    index += 1;
                }
                if words
                    .get(index)
                    .is_some_and(|word| word.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
                {
                    index += 1;
                }
            }
            _ => break,
        }
    }
    words.get(index).map(|_| index)
}

/// A conservative UX classification for the primary-checkout approval gate.
/// Every segment must be a known read-only invocation or a validation command
/// whose writes are build artifacts. Unknown syntax/commands are not called
/// read-only; the OS sandbox still supplies the actual containment boundary.
pub(crate) fn command_is_read_only(command: &str) -> bool {
    if command.contains('>') {
        return false;
    }
    !super::git::command_requires_metadata_write(command)
        && !any_command_segment(command, |words| {
            if words.is_empty() {
                return false;
            }
            let Some(index) = executable_index(words) else {
                return true;
            };
            let Some(program) = words.get(index).map(String::as_str) else {
                return true;
            };
            let name = program.rsplit('/').next().unwrap_or(program);
            let read_only = match name {
                "git" => true,
                "cargo" => matches!(
                    words.get(index + 1).map(String::as_str),
                    Some(
                        "check"
                            | "build"
                            | "test"
                            | "nextest"
                            | "clippy"
                            | "fmt"
                            | "run"
                            | "doc"
                            | "tree"
                            | "metadata"
                            | "outdated"
                    )
                ),
                "ls" | "pwd" | "cat" | "grep" | "rg" | "find" | "head" | "tail" | "wc" | "sort"
                | "uniq" | "cut" | "tr" | "jq" | "echo" | "printf" | "date" | "env" | "which"
                | "type" | "ps" | "pgrep" | "stat" | "file" | "ldd" | "readlink" | "realpath"
                | "dirname" | "basename" | "uname" | "nproc" | "du" | "df" | "tree" | "true"
                | "false" | "test" | "[" => true,
                "sed" => !words[index + 1..].iter().any(|word| {
                    word == "-i" || word.starts_with("-i") || word.starts_with("--in-place")
                }),
                _ => false,
            };
            !read_only
        })
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

/// Small shell lexer used only as a proactive UX classifier. It preserves
/// quoted words and command boundaries without trying to execute expansions.
/// Unsupported shell constructs can only cause the generic sandbox-denial
/// fallback; they never widen access.
pub(super) fn tokenize(command: &str) -> Vec<ShellToken> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut chars = command.chars().peekable();
    let mut quote = None;

    let push_word = |tokens: &mut Vec<ShellToken>, word: &mut String| {
        if !word.is_empty() {
            tokens.push(ShellToken::Word(std::mem::take(word)));
        }
    };

    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    word.push(ch);
                }
            }
            Some('"') => match ch {
                '"' => quote = None,
                '\\' => {
                    if let Some(next) = chars.next() {
                        word.push(next);
                    }
                }
                _ => word.push(ch),
            },
            Some(_) => unreachable!(),
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '\\' => {
                    if let Some(next) = chars.next() {
                        word.push(next);
                    }
                }
                ' ' | '\t' | '\r' => push_word(&mut tokens, &mut word),
                '\n' => {
                    push_word(&mut tokens, &mut word);
                    tokens.push(ShellToken::Separator(Separator::Newline));
                }
                ';' => {
                    push_word(&mut tokens, &mut word);
                    tokens.push(ShellToken::Separator(Separator::Semicolon));
                }
                '(' => {
                    push_word(&mut tokens, &mut word);
                    tokens.push(ShellToken::Separator(Separator::OpenParen));
                }
                ')' => {
                    push_word(&mut tokens, &mut word);
                    tokens.push(ShellToken::Separator(Separator::CloseParen));
                }
                '|' => {
                    push_word(&mut tokens, &mut word);
                    if chars.peek() == Some(&'|') {
                        chars.next();
                        tokens.push(ShellToken::Separator(Separator::Or));
                    } else {
                        tokens.push(ShellToken::Separator(Separator::Pipe));
                    }
                }
                '&' => {
                    push_word(&mut tokens, &mut word);
                    if chars.peek() == Some(&'&') {
                        chars.next();
                        tokens.push(ShellToken::Separator(Separator::And));
                    } else {
                        tokens.push(ShellToken::Separator(Separator::Background));
                    }
                }
                '#' if word.is_empty() => {
                    for next in chars.by_ref() {
                        if next == '\n' {
                            tokens.push(ShellToken::Separator(Separator::Newline));
                            break;
                        }
                    }
                }
                _ => word.push(ch),
            },
        }
    }
    push_word(&mut tokens, &mut word);
    tokens
}

#[cfg(test)]
mod tests {
    use super::command_is_read_only;

    #[test]
    fn read_only_and_validation_commands_stay_out_of_the_primary_gate() {
        for command in [
            "git status --short",
            "git diff --stat | head -20",
            "cargo check --workspace",
            "cargo nextest run --workspace --locked",
            "env -u HORIZON_AGENTD_BINARY cargo test -p horizon-agent",
            "nice -n 19 cargo clippy --workspace",
            "timeout 300 cargo test",
            "sed -n '1,80p' src/lib.rs",
            "echo alive",
        ] {
            assert!(command_is_read_only(command), "{command}");
        }
    }

    #[test]
    fn mutation_or_unrecognized_commands_require_the_primary_gate() {
        for command in [
            "git add src/lib.rs",
            "cargo update",
            "sed -i 's/a/b/' src/lib.rs",
            "echo change > src/lib.rs",
            "python3 analyze.py",
            "cargo test && sed -i 's/a/b/' src/lib.rs",
        ] {
            assert!(!command_is_read_only(command), "{command}");
        }
    }
}

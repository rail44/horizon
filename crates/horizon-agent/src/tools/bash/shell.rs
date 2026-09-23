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
            _ => break,
        }
    }
    words.get(index).map(|_| index)
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

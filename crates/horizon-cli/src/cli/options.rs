//! Scan flags independently of subcommand arity and command construction.
//! The caller consumes supported options before checking the remaining ones.

use std::path::PathBuf;

use super::{GlobalOptions, SplitFlag, UsageError};

pub(super) struct Arguments {
    pub(super) global: GlobalOptions,
    pub(super) options: CommandOptions,
    pub(super) positionals: Vec<String>,
}

#[derive(Default)]
pub(super) struct CommandOptions {
    pub(super) prompt: Option<String>,
    pub(super) split: Option<SplitFlag>,
    pub(super) active: bool,
    pub(super) reason: Option<String>,
}

impl CommandOptions {
    pub(super) fn validate_unused(&self) -> Result<(), UsageError> {
        for (flag, given) in [
            ("--prompt", self.prompt.is_some()),
            ("--split", self.split.is_some()),
        ] {
            if given {
                return Err(UsageError(format!("{flag} is only valid with new")));
            }
        }
        if self.active {
            return Err(UsageError(
                "--active is only valid with new/attach".to_string(),
            ));
        }

        Ok(())
    }
}

pub(super) fn scan(args: &[String]) -> Result<Arguments, UsageError> {
    let mut parsed = Arguments {
        global: GlobalOptions {
            socket: None,
            json: false,
            yes: false,
        },
        options: CommandOptions::default(),
        positionals: Vec::new(),
    };
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let (flag, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(name, value)| (name, Some(value)));
        match flag {
            "--socket" => {
                parsed.global.socket = Some(PathBuf::from(required_value(flag, inline, &mut iter)?))
            }
            "--json" if inline.is_none() => parsed.global.json = true,
            "--yes" if inline.is_none() => parsed.global.yes = true,
            "--active" if inline.is_none() => parsed.options.active = true,
            "--reason" => parsed.options.reason = Some(required_value(flag, inline, &mut iter)?),
            "--prompt" => parsed.options.prompt = Some(required_value(flag, inline, &mut iter)?),
            "--split" => {
                parsed.options.split = Some(match inline {
                    Some(value) => SplitFlag::Explicit(value.to_string()),
                    None => match iter.peek() {
                        Some(next) if !next.starts_with("--") => {
                            SplitFlag::Explicit(iter.next().expect("peeked Some").clone())
                        }
                        _ => SplitFlag::Here,
                    },
                });
            }
            _ if arg.starts_with("--") => {
                return Err(UsageError(format!("unrecognized flag: {arg}")));
            }
            _ => parsed.positionals.push(arg.clone()),
        }
    }
    Ok(parsed)
}

fn required_value<'a>(
    flag: &str,
    inline: Option<&'a str>,
    args: &mut impl Iterator<Item = &'a String>,
) -> Result<String, UsageError> {
    inline
        .map(str::to_owned)
        .or_else(|| args.next().cloned())
        .ok_or_else(|| UsageError(format!("{flag} requires a value")))
}

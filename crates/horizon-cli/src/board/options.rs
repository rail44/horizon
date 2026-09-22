//! Board flag parsing, independent of store discovery and command execution.

#[derive(Debug, Default)]
pub(super) struct Options {
    pub(super) positionals: Vec<String>,
    pub(super) body: Option<String>,
    pub(super) title: Option<String>,
    pub(super) parent: Option<String>,
    pub(super) after: Option<String>,
    pub(super) before: Option<String>,
    pub(super) top: bool,
    pub(super) status: Option<String>,
    pub(super) author: Option<String>,
    pub(super) since: Option<String>,
    pub(super) json: bool,
    pub(super) all: bool,
}

impl Options {
    pub(super) fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            let slot = match arg.as_str() {
                "--body" => &mut options.body,
                "--title" => &mut options.title,
                "--parent" => &mut options.parent,
                "--after" => &mut options.after,
                "--before" => &mut options.before,
                "--status" => &mut options.status,
                "--author" => &mut options.author,
                "--since" => &mut options.since,
                "--top" => {
                    options.top = true;
                    continue;
                }
                "--json" => {
                    options.json = true;
                    continue;
                }
                "--all" => {
                    options.all = true;
                    continue;
                }
                s if s.starts_with("--") => return Err(format!("unrecognized flag: {s}")),
                _ => {
                    options.positionals.push(arg.clone());
                    continue;
                }
            };
            *slot = Some(
                iter.next()
                    .ok_or_else(|| format!("{arg} requires a value"))?
                    .clone(),
            );
        }
        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use super::Options;

    fn parse(args: &[&str]) -> Result<Options, String> {
        Options::parse(&args.iter().map(|s| (*s).into()).collect::<Vec<_>>())
    }

    #[test]
    fn flags_interleave_with_positionals_and_last_value_wins() {
        let options = parse(&[
            "1", "--body", "old", "--top", "text", "--json", "--body", "new", "--all",
        ])
        .unwrap();
        assert_eq!(options.positionals, ["1", "text"]);
        assert_eq!(options.body.as_deref(), Some("new"));
        assert!(options.top && options.json && options.all);
    }

    #[test]
    fn a_flag_spelling_can_be_a_value_and_short_flags_remain_positionals() {
        let options = parse(&["--body", "--top", "-x"]).unwrap();
        assert_eq!(options.body.as_deref(), Some("--top"));
        assert!(!options.top);
        assert_eq!(options.positionals, ["-x"]);
    }

    #[test]
    fn missing_values_and_unknown_flags_keep_their_diagnostics() {
        for flag in [
            "--body", "--title", "--parent", "--after", "--before", "--status", "--author",
            "--since",
        ] {
            assert_eq!(
                parse(&[flag]).unwrap_err(),
                format!("{flag} requires a value")
            );
        }
        assert_eq!(
            parse(&["--unknown"]).unwrap_err(),
            "unrecognized flag: --unknown"
        );
    }
}

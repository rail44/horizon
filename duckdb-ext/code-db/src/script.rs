//! Reading SQL files the way the DuckDB CLI's `.read` does, for the two dot
//! commands the files here use: `.print` and `.read`. A line that starts
//! with `.` is a dot command; everything else is SQL.

use std::io::Write;
use std::path::Path;

use crate::db::Db;
use crate::render;

/// Where result sets and `.print` go: nowhere (loading), or stdout as markdown.
pub enum Output<'a> {
    Discard,
    Markdown(&'a mut dyn Write),
}

/// Split SQL text into statements at `;` outside quotes and comments. Each
/// statement keeps its leading comments; text with no SQL is dropped.
pub fn split_statements(sql: &str) -> Vec<String> {
    #[derive(PartialEq)]
    enum State {
        Code,
        Single,
        Double,
        Line,
        Block,
        Dollar,
    }
    let chars: Vec<char> = sql.chars().collect();
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut has_code = false;
    let mut state = State::Code;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match state {
            State::Code => match c {
                ';' => {
                    if has_code {
                        statements.push(current.trim().to_owned());
                    }
                    current.clear();
                    has_code = false;
                    i += 1;
                    continue;
                }
                '\'' => state = State::Single,
                '"' => state = State::Double,
                '-' if next == Some('-') => state = State::Line,
                '/' if next == Some('*') => state = State::Block,
                '$' if next == Some('$') => {
                    state = State::Dollar;
                    current.push_str("$$");
                    has_code = true;
                    i += 2;
                    continue;
                }
                _ => {}
            },
            State::Single if c == '\'' => state = State::Code,
            State::Double if c == '"' => state = State::Code,
            State::Line if c == '\n' => state = State::Code,
            State::Block if c == '*' && next == Some('/') => {
                current.push_str("*/");
                state = State::Code;
                i += 2;
                continue;
            }
            State::Dollar if c == '$' && next == Some('$') => {
                current.push_str("$$");
                state = State::Code;
                i += 2;
                continue;
            }
            _ => {}
        }
        if !c.is_whitespace() && !matches!(state, State::Line | State::Block) {
            has_code = true;
        }
        current.push(c);
        i += 1;
    }
    if has_code {
        statements.push(current.trim().to_owned());
    }
    statements
}

/// The arguments of a dot command, with single or double quotes removed.
fn dot_arguments(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&first) = chars.peek() else { break };
        let mut arg = String::new();
        if first == '\'' || first == '"' {
            chars.next();
            for c in chars.by_ref() {
                if c == first {
                    break;
                }
                arg.push(c);
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                arg.push(c);
                chars.next();
            }
        }
        args.push(arg);
    }
    args
}

/// Run a SQL file's text: statements in order, dot commands between them.
pub fn run_text(db: &Db, text: &str, output: &mut Output) -> Result<(), String> {
    let mut sql = String::new();
    for line in text.split_inclusive('\n') {
        if !line.starts_with('.') {
            sql.push_str(line);
            continue;
        }
        run_sql(db, &sql, output)?;
        sql.clear();
        let args = dot_arguments(line.trim_end());
        match args[0].as_str() {
            ".print" => {
                if let Output::Markdown(out) = output {
                    writeln!(out, "{}", args[1..].join(" ")).map_err(|e| e.to_string())?;
                }
            }
            ".read" if args.len() == 2 => run_file(db, Path::new(&args[1]), output)?,
            _ => {
                return Err(format!(
                    "unsupported dot command (the SQL files may use .print and .read): {}",
                    line.trim_end()
                ));
            }
        }
    }
    run_sql(db, &sql, output)
}

fn run_sql(db: &Db, sql: &str, output: &mut Output) -> Result<(), String> {
    match output {
        Output::Discard => db.run(sql, &mut |_| {}),
        Output::Markdown(out) => {
            for statement in split_statements(sql) {
                if let Some(table) = db.run_statement(&statement)? {
                    out.write_all(render::markdown(&table).as_bytes())
                        .map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        }
    }
}

pub fn run_file(db: &Db, path: &Path, output: &mut Output) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    run_text(db, &text, output).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_split_outside_quotes_and_comments() {
        let sql = "-- head; comment\nSELECT 'a;b' AS \"x;y\"; /* c; */ SELECT 2;\n-- tail;\n";
        assert_eq!(
            split_statements(sql),
            [
                "-- head; comment\nSELECT 'a;b' AS \"x;y\"",
                "/* c; */ SELECT 2"
            ]
        );
        assert_eq!(split_statements("SELECT $$;$$"), ["SELECT $$;$$"]);
        assert!(split_statements("-- only a comment;\n").is_empty());
    }

    #[test]
    fn dot_arguments_drop_quotes() {
        assert_eq!(
            dot_arguments(".print ## 1. 'a b'"),
            [".print", "##", "1.", "a b"]
        );
        assert_eq!(dot_arguments(".print"), [".print"]);
    }
}

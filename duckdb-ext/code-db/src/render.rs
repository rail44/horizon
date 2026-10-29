//! Result sets as the DuckDB 2.0.0 CLI prints them in `.mode markdown`: the
//! header centred, values left-aligned and padded to the column's display
//! width, `|` escaped, NULL as `NULL`, and a `:` in the separator of numeric
//! columns.

use std::fmt::Write;

use unicode_width::UnicodeWidthChar;

use crate::db::Table;

/// Display width the way the CLI measures it: East Asian wide characters
/// count two, a tab advances to the next multiple of eight, and a newline
/// starts the count again (a value with several lines is as wide as its last
/// line).
fn display_width(text: &str) -> usize {
    let last = text.rsplit('\n').next().unwrap_or("");
    last.chars().fold(0, |w, c| match c {
        '\t' => (w / 8 + 1) * 8,
        _ => w + c.width().unwrap_or(0),
    })
}

fn pad(out: &mut String, n: usize) {
    out.extend(std::iter::repeat_n(' ', n));
}

pub fn markdown(table: &Table) -> String {
    if table.explain {
        // The CLI prints an EXPLAIN result as its plan text (explain_value) alone.
        return table
            .rows
            .iter()
            .filter_map(|r| r.get(1).cloned().flatten())
            .map(|plan| {
                if plan.ends_with('\n') {
                    plan
                } else {
                    plan + "\n"
                }
            })
            .collect();
    }
    let cell = |v: &Option<String>| match v {
        Some(s) => s.replace('|', "\\|"),
        None => "NULL".to_owned(),
    };
    let header: Vec<String> = table.names.iter().map(|n| n.replace('|', "\\|")).collect();
    let rows: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|r| r.iter().map(cell).collect())
        .collect();
    let widths: Vec<usize> = (0..header.len())
        .map(|c| {
            rows.iter()
                .map(|r| display_width(&r[c]))
                .chain([display_width(&header[c])])
                .max()
                .unwrap_or(0)
        })
        .collect();

    let mut out = String::new();
    out.push('|');
    for (name, &w) in header.iter().zip(&widths) {
        let free = w - display_width(name);
        out.push(' ');
        pad(&mut out, free / 2);
        out.push_str(name);
        pad(&mut out, free - free / 2);
        out.push_str(" |");
    }
    out.push_str("\n|");
    for (&w, &numeric) in widths.iter().zip(&table.numeric) {
        out.extend(std::iter::repeat_n('-', w + 1));
        out.push(if numeric { ':' } else { '-' });
        out.push('|');
    }
    out.push('\n');
    for row in &rows {
        out.push('|');
        for (value, &w) in row.iter().zip(&widths) {
            let _ = write!(out, " {value}");
            pad(&mut out, w - display_width(value));
            out.push_str(" |");
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(names: &[&str], numeric: &[bool], rows: &[&[Option<&str>]]) -> Table {
        Table {
            names: names.iter().map(|s| s.to_string()).collect(),
            numeric: numeric.to_vec(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|v| v.map(str::to_owned)).collect())
                .collect(),
            explain: false,
        }
    }

    #[test]
    fn matches_the_cli() {
        // Expected text copied from `duckdb2 -batch` in `.mode markdown`.
        let t = table(
            &["a", "c", "d", "pi|pe", "x"],
            &[true, true, false, false, true],
            &[&[
                Some("1"),
                None,
                Some("héllo 日本語"),
                Some("a|b"),
                Some("10"),
            ]],
        );
        assert_eq!(
            markdown(&t),
            "| a |  c   |      d       | pi\\|pe | x  |\n\
             |--:|-----:|--------------|--------|---:|\n\
             | 1 | NULL | héllo 日本語 | a\\|b   | 10 |\n"
        );
        let empty = table(&["x"], &[true], &[]);
        assert_eq!(markdown(&empty), "| x |\n|--:|\n");
        let tab = table(&["t"], &[false], &[&[Some("a\tb")]]);
        assert_eq!(markdown(&tab), "|     t     |\n|-----------|\n| a\tb |\n");
        let lines = table(
            &["m", "n"],
            &[false, false],
            &[&[Some("multi\nline"), Some("ab\nlonger")]],
        );
        assert_eq!(
            markdown(&lines),
            "|  m   |   n    |\n|------|--------|\n| multi\nline | ab\nlonger |\n"
        );
    }
}

//! Print the rows of `read_scip`, `read_scip_symbols` or `read_scip_metadata`
//! as TSV, computed by `scip_index_core` without DuckDB, for comparing with
//! the extension (`scripts/scip_ext_parity.py`).
//!
//! Usage: scip-probe INDEX occurrences|symbols|metadata [--stats]
//!
//! Columns are the extension's, in its order. NULL is `\N`; booleans are
//! `true`/`false`; every string is escaped as the content of a JSON string
//! (so tabs and newlines cannot split a row), and a list is a JSON array of
//! strings with no spaces.

use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use scip_index_core::{
    self as scip, DEFINITION, FORWARD_DEFINITION, GENERATED, IMPORT, READ_ACCESS, TEST,
    WRITE_ACCESS,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The content of a JSON string for `value`, as Python's
/// `json.dumps(value, ensure_ascii=False)[1:-1]` writes it.
fn escape(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

fn list(out: &mut String, values: &[String]) {
    out.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        escape(out, value);
        out.push('"');
    }
    out.push(']');
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 || args.len() > 3 || (args.len() == 3 && args[2] != "--stats") {
        return Err("usage: scip-probe INDEX occurrences|symbols|metadata [--stats]".into());
    }
    let stats = args.len() == 3;
    let path = Path::new(&args[0]);
    let start = Instant::now();
    let mut out = BufWriter::new(std::io::stdout().lock());
    let mut line = String::new();
    let mut count = 0u64;
    match args[1].as_str() {
        "occurrences" => {
            for row in scip::occurrences(path)? {
                let row = row?;
                line.clear();
                escape(&mut line, &row.document);
                line.push('\t');
                escape(&mut line, &row.symbol);
                let r = row.range;
                line += &format!(
                    "\t{}\t{}\t{}\t{}\t{}",
                    r.start_line, r.start_char, r.end_line, r.end_char, row.roles
                );
                for role in [
                    DEFINITION,
                    IMPORT,
                    WRITE_ACCESS,
                    READ_ACCESS,
                    GENERATED,
                    TEST,
                    FORWARD_DEFINITION,
                ] {
                    line += if row.has_role(role) {
                        "\ttrue"
                    } else {
                        "\tfalse"
                    };
                }
                match row.enclosing {
                    Some(e) => {
                        line += &format!(
                            "\t{}\t{}\t{}\t{}",
                            e.start_line, e.start_char, e.end_line, e.end_char
                        )
                    }
                    None => line += "\t\\N\t\\N\t\\N\t\\N",
                }
                writeln!(out, "{line}")?;
                count += 1;
            }
        }
        "symbols" => {
            for row in scip::symbols(path)? {
                let row = row?;
                line.clear();
                match &row.document {
                    Some(document) => escape(&mut line, document),
                    None => line.push_str("\\N"),
                }
                line.push('\t');
                escape(&mut line, &row.symbol);
                line.push('\t');
                escape(&mut line, &scip::kind_name(row.kind));
                line.push('\t');
                escape(&mut line, &row.display_name);
                line.push('\t');
                escape(&mut line, &row.enclosing_symbol);
                line.push('\t');
                list(&mut line, &row.documentation);
                line += &format!("\t{}", row.relationship_count);
                writeln!(out, "{line}")?;
                count += 1;
            }
        }
        "metadata" => {
            let row = scip::metadata(path)?;
            line += &row.version.to_string();
            for value in [&row.tool_name, &row.tool_version] {
                line.push('\t');
                escape(&mut line, value);
            }
            line.push('\t');
            list(&mut line, &row.tool_arguments);
            line.push('\t');
            escape(&mut line, &row.project_root);
            line.push('\t');
            escape(
                &mut line,
                &scip::text_encoding_name(row.text_document_encoding),
            );
            line += &format!(
                "\t{}\t{}\t{}",
                row.document_count, row.occurrence_count, row.external_symbol_count
            );
            writeln!(out, "{line}")?;
            count = 1;
        }
        other => return Err(format!("unknown row set {other}").into()),
    }
    out.flush()?;
    if stats {
        eprintln!(
            "rows: {count}, time: {:.3} s",
            start.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

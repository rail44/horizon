//! Parity of the decoder with `scip print --json` of the `scip` CLI v0.10.0
//! (the Go reference decoder), as multisets of rows.
//!
//! Needs the CLI (`duckdb-ext/scripts/fetch-scip-cli.sh`, or `SCIP_CLI`) and,
//! for the fixture crate, `rust-analyzer` on `PATH`. The ignored test
//! `repository_index_parity` compares an index given in `SCIP_INDEX`.

use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use scip_index_core::{self as scip, MetadataRow, OccurrenceRow, Range, SymbolRow};
use serde::Deserialize;

fn scip_cli() -> PathBuf {
    let path = std::env::var_os("SCIP_CLI")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/tools/scip-v0.10.0/scip")
        });
    assert!(
        path.exists(),
        "{} is missing; run duckdb-ext/scripts/fetch-scip-cli.sh first",
        path.display()
    );
    path
}

// `scip print --json` is Go's encoding/json of the generated structs:
// snake_case names, zero values omitted, enums as numbers, and a oneof as an
// object keyed by the member's Go name.

#[derive(Deserialize, Default)]
#[serde(default)]
struct JsonIndex {
    metadata: JsonMetadata,
    documents: Vec<JsonDocument>,
    external_symbols: Vec<JsonSymbol>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JsonMetadata {
    version: i32,
    tool_info: JsonTool,
    project_root: String,
    text_document_encoding: i32,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JsonTool {
    name: String,
    version: String,
    arguments: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JsonDocument {
    relative_path: String,
    occurrences: Vec<JsonOccurrence>,
    symbols: Vec<JsonSymbol>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JsonOccurrence {
    range: Vec<i32>,
    #[serde(rename = "TypedRange")]
    typed_range: Option<JsonTypedRange>,
    symbol: String,
    symbol_roles: i32,
    enclosing_range: Vec<i32>,
    #[serde(rename = "TypedEnclosingRange")]
    typed_enclosing_range: Option<JsonTypedEnclosingRange>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JsonTypedRange {
    #[serde(rename = "SingleLineRange")]
    single: Option<JsonSingle>,
    #[serde(rename = "MultiLineRange")]
    multi: Option<JsonMulti>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JsonTypedEnclosingRange {
    #[serde(rename = "SingleLineEnclosingRange")]
    single: Option<JsonSingle>,
    #[serde(rename = "MultiLineEnclosingRange")]
    multi: Option<JsonMulti>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JsonSingle {
    line: i32,
    start_character: i32,
    end_character: i32,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JsonMulti {
    start_line: i32,
    start_character: i32,
    end_line: i32,
    end_character: i32,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JsonSymbol {
    symbol: String,
    documentation: Vec<String>,
    relationships: Vec<serde::de::IgnoredAny>,
    kind: i32,
    display_name: String,
    enclosing_symbol: String,
}

fn typed(single: Option<JsonSingle>, multi: Option<JsonMulti>) -> Option<Range> {
    match (single, multi) {
        (Some(s), _) => Some(Range {
            start_line: s.line,
            start_char: s.start_character,
            end_line: s.line,
            end_char: s.end_character,
        }),
        (None, Some(m)) => Some(Range {
            start_line: m.start_line,
            start_char: m.start_character,
            end_line: m.end_line,
            end_char: m.end_character,
        }),
        (None, None) => None,
    }
}

fn packed(values: &[i32]) -> Option<Range> {
    match *values {
        [line, start, end] => Some(Range {
            start_line: line,
            start_char: start,
            end_line: line,
            end_char: end,
        }),
        [start_line, start_char, end_line, end_char] => Some(Range {
            start_line,
            start_char,
            end_line,
            end_char,
        }),
        [] => None,
        _ => panic!("reference range of {} elements", values.len()),
    }
}

fn symbol_row(symbol: JsonSymbol, document: Option<&Arc<str>>) -> SymbolRow {
    SymbolRow {
        document: document.cloned(),
        symbol: symbol.symbol,
        kind: symbol.kind,
        display_name: symbol.display_name,
        enclosing_symbol: symbol.enclosing_symbol,
        documentation: symbol.documentation,
        relationship_count: symbol.relationships.len() as i32,
    }
}

struct Rows {
    occurrences: Vec<OccurrenceRow>,
    symbols: Vec<SymbolRow>,
    metadata: MetadataRow,
}

/// The rows the three functions should return, from `scip print --json`.
fn reference(index: &Path) -> Rows {
    let mut child = Command::new(scip_cli())
        .args(["print", "--json"])
        .arg(index)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let json: JsonIndex = serde_json::from_reader(BufReader::with_capacity(
        1 << 20,
        child.stdout.take().unwrap(),
    ))
    .unwrap();
    assert!(child.wait().unwrap().success(), "scip print failed");
    let mut rows = Rows {
        occurrences: Vec::new(),
        symbols: Vec::new(),
        metadata: MetadataRow {
            version: json.metadata.version,
            tool_name: json.metadata.tool_info.name,
            tool_version: json.metadata.tool_info.version,
            tool_arguments: json.metadata.tool_info.arguments,
            project_root: json.metadata.project_root,
            text_document_encoding: json.metadata.text_document_encoding,
            document_count: json.documents.len() as u64,
            occurrence_count: 0,
            external_symbol_count: json.external_symbols.len() as u64,
        },
    };
    for document in json.documents {
        let path: Arc<str> = document.relative_path.into();
        rows.metadata.occurrence_count += document.occurrences.len() as u64;
        for o in document.occurrences {
            let range = o
                .typed_range
                .and_then(|t| typed(t.single, t.multi))
                .or_else(|| packed(&o.range))
                .expect("reference occurrence without a range");
            let enclosing = o
                .typed_enclosing_range
                .and_then(|t| typed(t.single, t.multi))
                .or_else(|| packed(&o.enclosing_range));
            rows.occurrences.push(OccurrenceRow {
                document: path.clone(),
                symbol: o.symbol,
                range,
                roles: o.symbol_roles,
                enclosing,
            });
        }
        rows.symbols.extend(
            document
                .symbols
                .into_iter()
                .map(|symbol| symbol_row(symbol, Some(&path))),
        );
    }
    rows.symbols.extend(
        json.external_symbols
            .into_iter()
            .map(|symbol| symbol_row(symbol, None)),
    );
    rows
}

fn ours(index: &Path) -> Rows {
    Rows {
        occurrences: scip::occurrences(index)
            .unwrap()
            .collect::<scip::Result<_>>()
            .unwrap(),
        symbols: scip::symbols(index)
            .unwrap()
            .collect::<scip::Result<_>>()
            .unwrap(),
        metadata: scip::metadata(index).unwrap(),
    }
}

/// The rows only in `left` and only in `right`, as multisets.
fn difference<T: Hash + Eq + Clone + Debug>(left: &[T], right: &[T]) -> (Vec<T>, Vec<T>) {
    let mut counts: HashMap<&T, i64> = HashMap::new();
    for row in left {
        *counts.entry(row).or_default() += 1;
    }
    for row in right {
        *counts.entry(row).or_default() -= 1;
    }
    let mut only_left = Vec::new();
    let mut only_right = Vec::new();
    for (row, count) in counts {
        for _ in 0..count.max(0) {
            only_left.push(row.clone());
        }
        for _ in 0..(-count).max(0) {
            only_right.push(row.clone());
        }
    }
    (only_left, only_right)
}

fn assert_same<T: Hash + Eq + Clone + Debug>(what: &str, ours: &[T], reference: &[T]) {
    let (only_ours, only_reference) = difference(ours, reference);
    eprintln!(
        "{what}: {} rows, reference {} rows, only ours {}, only reference {}",
        ours.len(),
        reference.len(),
        only_ours.len(),
        only_reference.len()
    );
    assert!(
        only_ours.is_empty() && only_reference.is_empty(),
        "{what}: only ours {:?}, only reference {:?}",
        &only_ours[..only_ours.len().min(5)],
        &only_reference[..only_reference.len().min(5)]
    );
}

fn assert_parity(index: &Path) -> Rows {
    let ours = ours(index);
    let reference = reference(index);
    assert_same("occurrences", &ours.occurrences, &reference.occurrences);
    assert_same("symbols", &ours.symbols, &reference.symbols);
    assert_eq!(ours.metadata, reference.metadata);
    eprintln!("metadata: {:?}", ours.metadata);
    ours
}

const FIXTURE_MAIN: &str = r#"//! The fixture crate.
//!
//! Its docs span lines.

mod shapes;

use shapes::{Area, Square};
use std::collections::HashMap;

macro_rules! twice {
    ($e:expr) => {
        $e * 2
    };
}

/// A counter.
pub struct Counter {
    pub count: u32,
}

impl Counter {
    /// Adds one.
    pub fn bump(&mut self) -> u32 {
        self.count += 1;
        self.count
    }
}

fn main() {
    let mut counter = Counter { count: 0 };
    let mut seen: HashMap<String, u32> = HashMap::new();
    let label = "größe";
    for _ in 0..twice!(2) {
        let n = counter.bump();
        seen.insert(label.to_string(), n);
    }
    let square = Square { side: 3.0 };
    println!("{} {:?} {}", counter.count, seen, square.area());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bumps() {
        let mut c = Counter { count: 1 };
        assert_eq!(c.bump(), 2);
    }
}
"#;

const FIXTURE_SHAPES: &str = r#"/// Something with an area.
pub trait Area {
    /// The area.
    fn area(&self) -> f64;
}

/// A square.
#[derive(Debug, Clone)]
pub struct Square {
    pub side: f64,
}

impl Area for Square {
    fn area(&self) -> f64 {
        let s = self.side;
        s * s
    }
}

pub enum Shape {
    Square(Square),
    Empty,
}
"#;

#[test]
fn rust_analyzer_fixture_matches_scip_print() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(root.join("src/main.rs"), FIXTURE_MAIN).unwrap();
    std::fs::write(root.join("src/shapes.rs"), FIXTURE_SHAPES).unwrap();
    let index = root.join("index.scip");
    let output = Command::new("rust-analyzer")
        .args(["scip", "."])
        .arg("--output")
        .arg(&index)
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .output()
        .expect("rust-analyzer must be on PATH (rustup component add rust-analyzer)");
    assert!(
        output.status.success(),
        "rust-analyzer scip failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let rows = assert_parity(&index);

    // The fixture exercises what the columns distinguish.
    let occurrences = &rows.occurrences;
    let documents: std::collections::BTreeSet<&str> =
        occurrences.iter().map(|o| &*o.document).collect();
    assert_eq!(
        documents.into_iter().collect::<Vec<_>>(),
        ["src/main.rs", "src/shapes.rs"]
    );
    assert!(occurrences.iter().any(|o| o.symbol.starts_with("local ")));
    assert!(occurrences.iter().any(|o| o.has_role(scip::DEFINITION)));
    assert!(occurrences.iter().any(|o| o.roles == 0));
    assert!(occurrences.iter().any(|o| o.enclosing.is_some()));
    assert!(occurrences.iter().any(|o| o.enclosing.is_none()));
    assert!(
        occurrences
            .iter()
            .any(|o| o.range.end_line != o.range.start_line)
    );
    assert!(
        rows.symbols
            .iter()
            .any(|s| s.documentation.iter().any(|d| d.contains('\n')))
    );
    assert!(rows.symbols.iter().any(|s| s.documentation.is_empty()));
    // rust-analyzer 1.99.1 writes no relationships; the hand-encoded index covers them.
    assert_eq!(rows.metadata.tool_name, "rust-analyzer");
    assert_eq!(rows.metadata.document_count, 2);
}

/// A protobuf encoder for the hand-made index.
#[derive(Default)]
struct Msg(Vec<u8>);

impl Msg {
    fn key(mut self, number: u32, wire: u8) -> Self {
        let mut value = (u64::from(number) << 3) | u64::from(wire);
        while value >= 0x80 {
            self.0.push((value as u8) | 0x80);
            value >>= 7;
        }
        self.0.push(value as u8);
        self
    }

    fn varint(mut self, mut value: u64) -> Self {
        while value >= 0x80 {
            self.0.push((value as u8) | 0x80);
            value >>= 7;
        }
        self.0.push(value as u8);
        self
    }

    fn int(self, number: u32, value: i32) -> Self {
        self.key(number, 0).varint(value as i64 as u64)
    }

    fn bytes(self, number: u32, value: &[u8]) -> Self {
        let mut msg = self.key(number, 2).varint(value.len() as u64);
        msg.0.extend_from_slice(value);
        msg
    }

    fn str(self, number: u32, value: &str) -> Self {
        self.bytes(number, value.as_bytes())
    }

    fn msg(self, number: u32, value: Msg) -> Self {
        self.bytes(number, &value.0)
    }

    fn packed(self, number: u32, values: &[i32]) -> Self {
        let payload = values.iter().fold(Msg::default(), |msg, &value| {
            msg.varint(value as i64 as u64)
        });
        self.bytes(number, &payload.0)
    }
}

fn ints(values: &[i32]) -> Msg {
    values
        .iter()
        .enumerate()
        .fold(Msg::default(), |msg, (index, &value)| {
            msg.int(index as u32 + 1, value)
        })
}

/// The encodings rust-analyzer does not produce: typed ranges, typed
/// enclosing ranges, every role bit, negative values, unknown fields and
/// external symbols, decoded by the Go reference too.
#[test]
fn hand_encoded_index_matches_scip_print() {
    let occurrences = [
        Msg::default()
            .msg(8, ints(&[3, 1, 7]))
            .str(2, "s1")
            .int(3, 0x7f)
            .msg(11, ints(&[1, 0, 4, 2])),
        Msg::default()
            .msg(9, ints(&[3, 1, 5, 2]))
            .str(2, "local 1")
            .msg(10, ints(&[2, 0, 9])),
        Msg::default()
            .packed(1, &[4, 0, 6])
            .str(2, "s2")
            .int(3, 0x40 | 0x4)
            .packed(7, &[4, 0, 8, 1])
            .str(4, "override")
            .int(5, 6),
        Msg::default()
            .packed(1, &[5, 0, 5, 3])
            .msg(8, ints(&[9, 1, 2]))
            .str(2, "typed wins")
            .int(3, -1),
        // An occurrence with no symbol: SCIP allows syntax-only occurrences.
        Msg::default().packed(1, &[6, 0, 1]).int(5, 1),
    ];
    let mut document = Msg::default().str(4, "rust");
    for occurrence in occurrences {
        document = document.msg(2, occurrence);
    }
    let document = document
        .str(1, "src/ä.rs")
        .msg(
            3,
            Msg::default()
                .str(1, "s1")
                .str(3, "one")
                .str(3, "two\tlines\n")
                .int(5, 17)
                .str(6, "s1"),
        )
        .msg(3, Msg::default().str(1, "local 1").int(5, 61))
        .int(6, 1)
        .key(40, 1)
        .varint(0) // eight bytes of a fixed64 unknown field follow
        .varint(0)
        .varint(0)
        .varint(0)
        .varint(0)
        .varint(0)
        .varint(0)
        .varint(0);
    let metadata = Msg::default()
        .msg(
            2,
            Msg::default()
                .str(1, "hand")
                .str(2, "0")
                .str(3, "a")
                .str(3, "b"),
        )
        .str(3, "file:///p")
        .int(4, 1);
    let index = Msg::default()
        .msg(1, metadata)
        .msg(2, document)
        .msg(2, Msg::default().str(1, "empty.rs"))
        .msg(
            3,
            Msg::default()
                .str(1, "ext a")
                .int(5, 1000)
                .msg(4, Msg::default().str(1, "x").int(2, 1))
                .msg(4, Msg::default().str(1, "y"))
                .str(8, "enc"),
        )
        .msg(3, Msg::default().str(1, "ext b"))
        .str(99, "unknown top-level field");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hand.scip");
    std::fs::write(&path, &index.0).unwrap();

    let rows = assert_parity(&path);
    assert_eq!(rows.occurrences.len(), 5);
    assert_eq!(rows.symbols.len(), 4);
    assert_eq!(rows.metadata.external_symbol_count, 2);
}

#[test]
#[ignore = "needs SCIP_INDEX, e.g. this repository's rust-analyzer index"]
fn repository_index_parity() {
    let index = std::env::var_os("SCIP_INDEX").expect("SCIP_INDEX is not set");
    assert_parity(Path::new(&index));
}

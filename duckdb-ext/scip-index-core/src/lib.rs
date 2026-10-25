//! A streaming reader for SCIP code-intelligence indexes (binary protobuf),
//! producing the rows of the `scip_index` extension's three table functions.
//!
//! The decoder is hand-written against the protobuf wire format (no generated
//! code, no dependencies). Field numbers and enum values follow `scip.proto`
//! of `scip-code/scip` v0.10.0 (Apache-2.0), commit
//! `1c2b6db7e560d5233c944f36e4ac1377cc6963fc`:
//!
//! | Message | Fields read |
//! | --- | --- |
//! | `Index` | `metadata = 1`, `documents = 2`, `external_symbols = 3` |
//! | `Metadata` | `version = 1` (enum `ProtocolVersion`), `tool_info = 2`, `project_root = 3`, `text_document_encoding = 4` (enum `TextEncoding`) |
//! | `ToolInfo` | `name = 1`, `version = 2`, `arguments = 3` (repeated) |
//! | `Document` | `relative_path = 1`, `occurrences = 2`, `symbols = 3` |
//! | `Occurrence` | `range = 1` (repeated int32, deprecated), `symbol = 2`, `symbol_roles = 3`, `enclosing_range = 7` (repeated int32, deprecated), oneof `typed_range` { `single_line_range = 8`, `multi_line_range = 9` }, oneof `typed_enclosing_range` { `single_line_enclosing_range = 10`, `multi_line_enclosing_range = 11` } |
//! | `SingleLineRange` | `line = 1`, `start_character = 2`, `end_character = 3` |
//! | `MultiLineRange` | `start_line = 1`, `start_character = 2`, `end_line = 3`, `end_character = 4` |
//! | `SymbolInformation` | `symbol = 1`, `documentation = 3` (repeated), `relationships = 4` (repeated, counted), `kind = 5` (enum `Kind`), `display_name = 6`, `enclosing_symbol = 8` |
//!
//! Decoding follows protobuf's rules where they matter here: unknown fields,
//! and known fields with an unexpected wire type, are skipped; a `repeated
//! int32` is read packed or unpacked; a singular message field that occurs
//! more than once is merged; within a oneof the last member wins. As
//! `scip.proto` specifies, a typed range takes precedence over the deprecated
//! packed one, and a three-element packed range `[line, start, end]` has its
//! end line equal to its start line.
//!
//! Streaming: an `Index` is one message whose `documents` and
//! `external_symbols` are top-level length-delimited fields, so the reader
//! holds one top-level field (one document) in memory at a time.

mod wire;

use std::borrow::Cow;
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;
use std::sync::Arc;

use wire::{Fields, Value};

/// `SymbolRole` bits of `Occurrence.symbol_roles`.
pub const DEFINITION: i32 = 0x1;
pub const IMPORT: i32 = 0x2;
pub const WRITE_ACCESS: i32 = 0x4;
pub const READ_ACCESS: i32 = 0x8;
pub const GENERATED: i32 = 0x10;
pub const TEST: i32 = 0x20;
pub const FORWARD_DEFINITION: i32 = 0x40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    fn context(self, context: impl fmt::Display) -> Self {
        Self(format!("{context}: {}", self.0))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A half-open range, 0-based lines and characters as SCIP encodes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Range {
    pub start_line: i32,
    pub start_char: i32,
    pub end_line: i32,
    pub end_char: i32,
}

/// One `Occurrence` of a document.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OccurrenceRow {
    /// The document's `relative_path`.
    pub document: Arc<str>,
    /// The raw symbol string (`local N` for document-local symbols).
    pub symbol: String,
    pub range: Range,
    /// The raw `symbol_roles` bitmask; see the role constants.
    pub roles: i32,
    pub enclosing: Option<Range>,
}

impl OccurrenceRow {
    pub fn has_role(&self, role: i32) -> bool {
        self.roles & role != 0
    }
}

/// One `SymbolInformation`, of a document or of `Index.external_symbols`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SymbolRow {
    /// The document's `relative_path`; `None` for an external symbol.
    pub document: Option<Arc<str>>,
    pub symbol: String,
    /// The raw `Kind` enum value; [`kind_name`] gives its name.
    pub kind: i32,
    pub display_name: String,
    pub enclosing_symbol: String,
    pub documentation: Vec<String>,
    pub relationship_count: i32,
}

/// The index's `Metadata` and the counts of its top-level parts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct MetadataRow {
    /// The raw `ProtocolVersion` enum value.
    pub version: i32,
    pub tool_name: String,
    pub tool_version: String,
    pub tool_arguments: Vec<String>,
    pub project_root: String,
    /// The raw `TextEncoding` enum value; [`text_encoding_name`] gives its name.
    pub text_document_encoding: i32,
    pub document_count: u64,
    pub occurrence_count: u64,
    pub external_symbol_count: u64,
}

/// The name of a `SymbolInformation.Kind` value, or the number in decimal for
/// a value this version of `scip.proto` does not define.
pub fn kind_name(kind: i32) -> Cow<'static, str> {
    match KINDS.binary_search_by_key(&kind, |&(value, _)| value) {
        Ok(index) => Cow::Borrowed(KINDS[index].1),
        Err(_) => Cow::Owned(kind.to_string()),
    }
}

/// The name of a `TextEncoding` value, or the number in decimal.
pub fn text_encoding_name(encoding: i32) -> Cow<'static, str> {
    match encoding {
        0 => Cow::Borrowed("UnspecifiedTextEncoding"),
        1 => Cow::Borrowed("UTF8"),
        2 => Cow::Borrowed("UTF16"),
        other => Cow::Owned(other.to_string()),
    }
}

/// `SymbolInformation.Kind`, sorted by value.
const KINDS: &[(i32, &str)] = &[
    (0, "UnspecifiedKind"),
    (1, "Array"),
    (2, "Assertion"),
    (3, "AssociatedType"),
    (4, "Attribute"),
    (5, "Axiom"),
    (6, "Boolean"),
    (7, "Class"),
    (8, "Constant"),
    (9, "Constructor"),
    (10, "DataFamily"),
    (11, "Enum"),
    (12, "EnumMember"),
    (13, "Event"),
    (14, "Fact"),
    (15, "Field"),
    (16, "File"),
    (17, "Function"),
    (18, "Getter"),
    (19, "Grammar"),
    (20, "Instance"),
    (21, "Interface"),
    (22, "Key"),
    (23, "Lang"),
    (24, "Lemma"),
    (25, "Macro"),
    (26, "Method"),
    (27, "MethodReceiver"),
    (28, "Message"),
    (29, "Module"),
    (30, "Namespace"),
    (31, "Null"),
    (32, "Number"),
    (33, "Object"),
    (34, "Operator"),
    (35, "Package"),
    (36, "PackageObject"),
    (37, "Parameter"),
    (38, "ParameterLabel"),
    (39, "Pattern"),
    (40, "Predicate"),
    (41, "Property"),
    (42, "Protocol"),
    (43, "Quasiquoter"),
    (44, "SelfParameter"),
    (45, "Setter"),
    (46, "Signature"),
    (47, "Subscript"),
    (48, "String"),
    (49, "Struct"),
    (50, "Tactic"),
    (51, "Theorem"),
    (52, "ThisParameter"),
    (53, "Trait"),
    (54, "Type"),
    (55, "TypeAlias"),
    (56, "TypeClass"),
    (57, "TypeFamily"),
    (58, "TypeParameter"),
    (59, "Union"),
    (60, "Value"),
    (61, "Variable"),
    (62, "Contract"),
    (63, "Error"),
    (64, "Library"),
    (65, "Modifier"),
    (66, "AbstractMethod"),
    (67, "MethodSpecification"),
    (68, "ProtocolMethod"),
    (69, "PureVirtualMethod"),
    (70, "TraitMethod"),
    (71, "TypeClassMethod"),
    (72, "Accessor"),
    (73, "Delegate"),
    (74, "MethodAlias"),
    (75, "SingletonClass"),
    (76, "SingletonMethod"),
    (77, "StaticDataMember"),
    (78, "StaticEvent"),
    (79, "StaticField"),
    (80, "StaticMethod"),
    (81, "StaticProperty"),
    (82, "StaticVariable"),
    (84, "Extension"),
    (85, "Mixin"),
    (86, "Concept"),
];

/// A top-level field of `Index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Metadata,
    Document,
    ExternalSymbol,
}

/// Reads the top-level fields of an `Index` one at a time.
struct IndexReader<R> {
    input: R,
    buffer: Vec<u8>,
}

impl<R: Read> IndexReader<R> {
    fn new(input: R) -> Self {
        Self {
            input,
            buffer: Vec::new(),
        }
    }

    /// The next metadata, document or external symbol, skipping every other
    /// field; its bytes are valid until the next call.
    fn next_part(&mut self) -> Result<Option<(Part, &[u8])>> {
        loop {
            let Some(key) = wire::read_varint(&mut self.input)? else {
                return Ok(None);
            };
            let (number, wire_type) = wire::split_key(key)?;
            match wire_type {
                wire::VARINT => {
                    wire::read_varint(&mut self.input)?
                        .ok_or_else(|| Error::new("truncated index"))?;
                }
                wire::FIXED64 => self.skip(8)?,
                wire::FIXED32 => self.skip(4)?,
                _ => {
                    let len = wire::read_varint(&mut self.input)?
                        .ok_or_else(|| Error::new("truncated index"))?;
                    let part = match number {
                        1 => Part::Metadata,
                        2 => Part::Document,
                        3 => Part::ExternalSymbol,
                        _ => {
                            self.skip(len)?;
                            continue;
                        }
                    };
                    let len = usize::try_from(len)
                        .map_err(|_| Error::new("field length out of range"))?;
                    self.buffer.clear();
                    let read = (&mut self.input)
                        .take(len as u64)
                        .read_to_end(&mut self.buffer)?;
                    if read != len {
                        return Err(Error::new("truncated index"));
                    }
                    return Ok(Some((part, &self.buffer)));
                }
            }
        }
    }

    fn skip(&mut self, len: u64) -> Result<()> {
        let skipped = io::copy(&mut (&mut self.input).take(len), &mut io::sink())?;
        if skipped != len {
            return Err(Error::new("truncated index"));
        }
        Ok(())
    }
}

fn open(path: &Path) -> Result<BufReader<File>> {
    File::open(path)
        .map(|file| BufReader::with_capacity(1 << 16, file))
        .map_err(|error| Error::from(error).context(path.display()))
}

/// The occurrences of an index, document by document in file order.
pub struct Occurrences<R> {
    reader: IndexReader<R>,
    pending: std::vec::IntoIter<OccurrenceRow>,
    done: bool,
}

/// Opens `path` and iterates over its occurrences.
pub fn occurrences(path: &Path) -> Result<Occurrences<BufReader<File>>> {
    Ok(occurrences_from(open(path)?))
}

pub fn occurrences_from<R: Read>(input: R) -> Occurrences<R> {
    Occurrences {
        reader: IndexReader::new(input),
        pending: Vec::new().into_iter(),
        done: false,
    }
}

impl<R: Read> Iterator for Occurrences<R> {
    type Item = Result<OccurrenceRow>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(row) = self.pending.next() {
                return Some(Ok(row));
            }
            if self.done {
                return None;
            }
            let rows = match self.reader.next_part() {
                Ok(Some((Part::Document, bytes))) => document_occurrences(bytes),
                Ok(Some(_)) => continue,
                Ok(None) => {
                    self.done = true;
                    return None;
                }
                Err(error) => Err(error),
            };
            match rows {
                Ok(rows) => self.pending = rows.into_iter(),
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

/// The symbol information of an index: each document's, and the external
/// symbols, in file order.
pub struct Symbols<R> {
    reader: IndexReader<R>,
    pending: std::vec::IntoIter<SymbolRow>,
    done: bool,
}

/// Opens `path` and iterates over its symbol information.
pub fn symbols(path: &Path) -> Result<Symbols<BufReader<File>>> {
    Ok(symbols_from(open(path)?))
}

pub fn symbols_from<R: Read>(input: R) -> Symbols<R> {
    Symbols {
        reader: IndexReader::new(input),
        pending: Vec::new().into_iter(),
        done: false,
    }
}

impl<R: Read> Iterator for Symbols<R> {
    type Item = Result<SymbolRow>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(row) = self.pending.next() {
                return Some(Ok(row));
            }
            if self.done {
                return None;
            }
            let rows = match self.reader.next_part() {
                Ok(Some((Part::Document, bytes))) => document_symbols(bytes),
                Ok(Some((Part::ExternalSymbol, bytes))) => symbol_information(bytes, None)
                    .map(|row| vec![row])
                    .map_err(|error| error.context("external symbol")),
                Ok(Some(_)) => continue,
                Ok(None) => {
                    self.done = true;
                    return None;
                }
                Err(error) => Err(error),
            };
            match rows {
                Ok(rows) => self.pending = rows.into_iter(),
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

/// Reads `path` to its end for its metadata and counts.
pub fn metadata(path: &Path) -> Result<MetadataRow> {
    metadata_from(open(path)?)
}

pub fn metadata_from<R: Read>(input: R) -> Result<MetadataRow> {
    let mut reader = IndexReader::new(input);
    let mut row = MetadataRow::default();
    while let Some((part, bytes)) = reader.next_part()? {
        match part {
            Part::Metadata => merge_metadata(&mut row, bytes)?,
            Part::Document => {
                row.document_count += 1;
                for field in Fields::new(bytes) {
                    if let (2, Value::Len(_)) = field? {
                        row.occurrence_count += 1;
                    }
                }
            }
            Part::ExternalSymbol => row.external_symbol_count += 1,
        }
    }
    Ok(row)
}

fn merge_metadata(row: &mut MetadataRow, bytes: &[u8]) -> Result<()> {
    for field in Fields::new(bytes) {
        match field? {
            (1, Value::Varint(value)) => row.version = wire::int32(value),
            (2, Value::Len(tool)) => {
                for field in Fields::new(tool) {
                    match field? {
                        (1, Value::Len(name)) => row.tool_name = wire::string(name)?,
                        (2, Value::Len(version)) => row.tool_version = wire::string(version)?,
                        (3, Value::Len(argument)) => {
                            row.tool_arguments.push(wire::string(argument)?)
                        }
                        _ => {}
                    }
                }
            }
            (3, Value::Len(root)) => row.project_root = wire::string(root)?,
            (4, Value::Varint(value)) => row.text_document_encoding = wire::int32(value),
            _ => {}
        }
    }
    Ok(())
}

/// A document's `relative_path`, wherever it occurs among its fields
/// (protobuf does not fix the order); the last one wins.
fn relative_path(bytes: &[u8]) -> Result<Arc<str>> {
    let mut path = None;
    for field in Fields::new(bytes) {
        if let (1, Value::Len(value)) = field? {
            path = Some(value);
        }
    }
    Ok(wire::string(path.unwrap_or_default())?.into())
}

fn document_occurrences(bytes: &[u8]) -> Result<Vec<OccurrenceRow>> {
    let document = relative_path(bytes)?;
    let in_document = |error: Error| error.context(format!("document {document:?}"));
    let mut rows = Vec::new();
    for field in Fields::new(bytes) {
        if let (2, Value::Len(occurrence)) = field.map_err(in_document)? {
            rows.push(decode_occurrence(occurrence, &document).map_err(in_document)?);
        }
    }
    Ok(rows)
}

fn document_symbols(bytes: &[u8]) -> Result<Vec<SymbolRow>> {
    let document = relative_path(bytes)?;
    let in_document = |error: Error| error.context(format!("document {document:?}"));
    let mut rows = Vec::new();
    for field in Fields::new(bytes) {
        if let (3, Value::Len(symbol)) = field.map_err(in_document)? {
            rows.push(symbol_information(symbol, Some(&document)).map_err(in_document)?);
        }
    }
    Ok(rows)
}

fn symbol_information(bytes: &[u8], document: Option<&Arc<str>>) -> Result<SymbolRow> {
    let mut row = SymbolRow {
        document: document.cloned(),
        symbol: String::new(),
        kind: 0,
        display_name: String::new(),
        enclosing_symbol: String::new(),
        documentation: Vec::new(),
        relationship_count: 0,
    };
    for field in Fields::new(bytes) {
        match field? {
            (1, Value::Len(value)) => row.symbol = wire::string(value)?,
            (3, Value::Len(value)) => row.documentation.push(wire::string(value)?),
            (4, Value::Len(_)) => row.relationship_count += 1,
            (5, Value::Varint(value)) => row.kind = wire::int32(value),
            (6, Value::Len(value)) => row.display_name = wire::string(value)?,
            (8, Value::Len(value)) => row.enclosing_symbol = wire::string(value)?,
            _ => {}
        }
    }
    Ok(row)
}

/// The member of a typed-range oneof seen last, with its fields merged.
#[derive(Clone, Copy)]
enum Typed {
    /// `[line, start_character, end_character]`
    Single([i32; 3]),
    /// `[start_line, start_character, end_line, end_character]`
    Multi([i32; 4]),
}

impl Typed {
    fn merge_single(typed: &mut Option<Typed>, bytes: &[u8]) -> Result<()> {
        let mut values = match *typed {
            Some(Typed::Single(values)) => values,
            _ => [0; 3],
        };
        merge_int32s(&mut values, bytes)?;
        *typed = Some(Typed::Single(values));
        Ok(())
    }

    fn merge_multi(typed: &mut Option<Typed>, bytes: &[u8]) -> Result<()> {
        let mut values = match *typed {
            Some(Typed::Multi(values)) => values,
            _ => [0; 4],
        };
        merge_int32s(&mut values, bytes)?;
        *typed = Some(Typed::Multi(values));
        Ok(())
    }

    fn range(self) -> Range {
        match self {
            Typed::Single([line, start, end]) => Range {
                start_line: line,
                start_char: start,
                end_line: line,
                end_char: end,
            },
            Typed::Multi([start_line, start_char, end_line, end_char]) => Range {
                start_line,
                start_char,
                end_line,
                end_char,
            },
        }
    }
}

/// Sets `values[n - 1]` from field `n` of a message of `int32` fields.
fn merge_int32s(values: &mut [i32], bytes: &[u8]) -> Result<()> {
    for field in Fields::new(bytes) {
        if let (number, Value::Varint(value)) = field?
            && let Some(slot) = values.get_mut(number as usize - 1)
        {
            *slot = wire::int32(value);
        }
    }
    Ok(())
}

/// A deprecated packed range: three elements `[line, start, end]` or four.
fn packed_range(values: &[i32]) -> Option<Range> {
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
        _ => None,
    }
}

fn decode_occurrence(bytes: &[u8], document: &Arc<str>) -> Result<OccurrenceRow> {
    let mut symbol = String::new();
    let mut roles = 0;
    let mut packed = Vec::new();
    let mut typed = None;
    let mut packed_enclosing = Vec::new();
    let mut typed_enclosing = None;
    for field in Fields::new(bytes) {
        match field? {
            (1, value) => wire::push_int32s(&mut packed, value)?,
            (2, Value::Len(value)) => symbol = wire::string(value)?,
            (3, Value::Varint(value)) => roles = wire::int32(value),
            (7, value) => wire::push_int32s(&mut packed_enclosing, value)?,
            (8, Value::Len(value)) => Typed::merge_single(&mut typed, value)?,
            (9, Value::Len(value)) => Typed::merge_multi(&mut typed, value)?,
            (10, Value::Len(value)) => Typed::merge_single(&mut typed_enclosing, value)?,
            (11, Value::Len(value)) => Typed::merge_multi(&mut typed_enclosing, value)?,
            _ => {}
        }
    }
    let range = match typed {
        Some(typed) => typed.range(),
        None => packed_range(&packed).ok_or_else(|| {
            Error::new(format!(
                "occurrence of {symbol:?} has no typed range and a range of {} elements",
                packed.len()
            ))
        })?,
    };
    let enclosing = match typed_enclosing {
        Some(typed) => Some(typed.range()),
        None if packed_enclosing.is_empty() => None,
        None => Some(packed_range(&packed_enclosing).ok_or_else(|| {
            Error::new(format!(
                "occurrence of {symbol:?} has an enclosing range of {} elements",
                packed_enclosing.len()
            ))
        })?),
    };
    Ok(OccurrenceRow {
        document: document.clone(),
        symbol,
        range,
        roles,
        enclosing,
    })
}

#[cfg(test)]
mod tests;

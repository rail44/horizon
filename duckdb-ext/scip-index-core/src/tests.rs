//! Decoding of hand-made protobuf bytes.

use super::*;

/// A minimal protobuf encoder for building test messages.
#[derive(Default, Clone)]
struct Msg(Vec<u8>);

fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

impl Msg {
    fn new() -> Self {
        Self::default()
    }

    fn key(mut self, number: u32, wire: u8) -> Self {
        varint(&mut self.0, (u64::from(number) << 3) | u64::from(wire));
        self
    }

    /// An int32 field; negative values are sign-extended to ten bytes.
    fn int(self, number: u32, value: i32) -> Self {
        let mut msg = self.key(number, 0);
        varint(&mut msg.0, value as i64 as u64);
        msg
    }

    fn bytes(self, number: u32, value: &[u8]) -> Self {
        let mut msg = self.key(number, 2);
        varint(&mut msg.0, value.len() as u64);
        msg.0.extend_from_slice(value);
        msg
    }

    fn str(self, number: u32, value: &str) -> Self {
        self.bytes(number, value.as_bytes())
    }

    fn msg(self, number: u32, value: &Msg) -> Self {
        self.bytes(number, &value.0)
    }

    fn packed(self, number: u32, values: &[i32]) -> Self {
        let mut payload = Vec::new();
        for &value in values {
            varint(&mut payload, value as i64 as u64);
        }
        self.bytes(number, &payload)
    }

    fn fixed64(self, number: u32) -> Self {
        let mut msg = self.key(number, 1);
        msg.0.extend_from_slice(&[0xab; 8]);
        msg
    }

    fn fixed32(self, number: u32) -> Self {
        let mut msg = self.key(number, 5);
        msg.0.extend_from_slice(&[0xcd; 4]);
        msg
    }
}

fn single(line: i32, start: i32, end: i32) -> Msg {
    Msg::new().int(1, line).int(2, start).int(3, end)
}

fn multi(start_line: i32, start: i32, end_line: i32, end: i32) -> Msg {
    Msg::new()
        .int(1, start_line)
        .int(2, start)
        .int(3, end_line)
        .int(4, end)
}

fn range(start_line: i32, start_char: i32, end_line: i32, end_char: i32) -> Range {
    Range {
        start_line,
        start_char,
        end_line,
        end_char,
    }
}

fn occurrences_of(index: &Msg) -> Vec<OccurrenceRow> {
    occurrences_from(index.0.as_slice())
        .collect::<Result<_>>()
        .unwrap()
}

fn one_occurrence(occurrence: Msg) -> Result<OccurrenceRow> {
    let index = Msg::new().msg(2, &Msg::new().str(1, "a.rs").msg(2, &occurrence));
    let mut rows = occurrences_from(index.0.as_slice()).collect::<Result<Vec<_>>>()?;
    assert_eq!(rows.len(), 1);
    Ok(rows.remove(0))
}

#[test]
fn packed_legacy_ranges_of_three_and_four_elements() {
    let three = one_occurrence(Msg::new().packed(1, &[4, 2, 9]).str(2, "s")).unwrap();
    assert_eq!(three.range, range(4, 2, 4, 9));
    let four = one_occurrence(Msg::new().packed(1, &[4, 2, 6, 1]).str(2, "s")).unwrap();
    assert_eq!(four.range, range(4, 2, 6, 1));
    // Unpacked encoding of the same repeated field.
    let unpacked = one_occurrence(Msg::new().int(1, 4).int(1, 2).int(1, 9)).unwrap();
    assert_eq!(unpacked.range, range(4, 2, 4, 9));
    // A packed field split over two occurrences concatenates.
    let split = one_occurrence(Msg::new().packed(1, &[4, 2]).packed(1, &[6, 1])).unwrap();
    assert_eq!(split.range, range(4, 2, 6, 1));
}

#[test]
fn typed_single_and_multi_line_ranges() {
    let single_line = one_occurrence(Msg::new().msg(8, &single(3, 1, 7))).unwrap();
    assert_eq!(single_line.range, range(3, 1, 3, 7));
    let multi_line = one_occurrence(Msg::new().msg(9, &multi(3, 1, 5, 2))).unwrap();
    assert_eq!(multi_line.range, range(3, 1, 5, 2));
    // Zero fields are omitted on the wire: an empty SingleLineRange is 0:0-0:0.
    let empty = one_occurrence(Msg::new().msg(8, &Msg::new())).unwrap();
    assert_eq!(empty.range, range(0, 0, 0, 0));
}

#[test]
fn typed_range_takes_precedence_and_last_oneof_member_wins() {
    let both = one_occurrence(
        Msg::new()
            .packed(1, &[9, 9, 9])
            .msg(8, &single(1, 2, 3))
            .packed(1, &[]),
    )
    .unwrap();
    assert_eq!(both.range, range(1, 2, 1, 3));
    let switched = one_occurrence(
        Msg::new()
            .msg(8, &single(1, 2, 3))
            .msg(9, &multi(4, 5, 6, 7)),
    )
    .unwrap();
    assert_eq!(switched.range, range(4, 5, 6, 7));
    // The same member twice merges field by field.
    let merged = one_occurrence(
        Msg::new()
            .msg(9, &multi(4, 5, 6, 7))
            .msg(9, &Msg::new().int(3, 8)),
    )
    .unwrap();
    assert_eq!(merged.range, range(4, 5, 8, 7));
}

#[test]
fn malformed_ranges_are_errors() {
    let error = one_occurrence(Msg::new().packed(1, &[1, 2]).str(2, "s")).unwrap_err();
    assert!(error.to_string().contains("range of 2 elements"), "{error}");
    let error = one_occurrence(Msg::new().str(2, "s")).unwrap_err();
    assert!(error.to_string().contains("range of 0 elements"), "{error}");
    assert!(error.to_string().contains("\"a.rs\""), "{error}");
    let error = one_occurrence(Msg::new().packed(1, &[1, 2, 3]).packed(7, &[1])).unwrap_err();
    assert!(
        error.to_string().contains("enclosing range of 1"),
        "{error}"
    );
}

#[test]
fn roles_bitmask() {
    let all =
        DEFINITION | IMPORT | WRITE_ACCESS | READ_ACCESS | GENERATED | TEST | FORWARD_DEFINITION;
    assert_eq!(all, 0x7f);
    let row = one_occurrence(Msg::new().packed(1, &[0, 0, 1]).int(3, 0x1 | 0x8 | 0x20)).unwrap();
    assert_eq!(row.roles, 0x29);
    assert!(row.has_role(DEFINITION) && row.has_role(READ_ACCESS) && row.has_role(TEST));
    assert!(!row.has_role(IMPORT) && !row.has_role(WRITE_ACCESS));
    assert!(!row.has_role(GENERATED) && !row.has_role(FORWARD_DEFINITION));
    // Bits beyond the defined roles are kept in the raw value.
    let high = one_occurrence(Msg::new().packed(1, &[0, 0, 1]).int(3, 0x100 | 0x2)).unwrap();
    assert_eq!(high.roles, 0x102);
    let none = one_occurrence(Msg::new().packed(1, &[0, 0, 1])).unwrap();
    assert_eq!(none.roles, 0);
}

#[test]
fn negative_int32_values() {
    let row = one_occurrence(Msg::new().packed(1, &[-1, 2, -3]).int(3, -1)).unwrap();
    assert_eq!(row.range, range(-1, 2, -1, -3));
    assert_eq!(row.roles, -1);
}

#[test]
fn enclosing_ranges() {
    let missing = one_occurrence(Msg::new().packed(1, &[1, 2, 3])).unwrap();
    assert_eq!(missing.enclosing, None);
    let packed = one_occurrence(Msg::new().packed(1, &[1, 2, 3]).packed(7, &[0, 0, 4, 1])).unwrap();
    assert_eq!(packed.enclosing, Some(range(0, 0, 4, 1)));
    let packed3 = one_occurrence(Msg::new().packed(1, &[1, 2, 3]).packed(7, &[1, 0, 9])).unwrap();
    assert_eq!(packed3.enclosing, Some(range(1, 0, 1, 9)));
    let typed_single =
        one_occurrence(Msg::new().packed(1, &[1, 2, 3]).msg(10, &single(1, 0, 8))).unwrap();
    assert_eq!(typed_single.enclosing, Some(range(1, 0, 1, 8)));
    let typed_multi = one_occurrence(
        Msg::new()
            .packed(1, &[1, 2, 3])
            .packed(7, &[7, 7, 7])
            .msg(11, &multi(0, 0, 3, 1)),
    )
    .unwrap();
    assert_eq!(typed_multi.enclosing, Some(range(0, 0, 3, 1)));
}

#[test]
fn unknown_fields_are_skipped_at_every_level() {
    let occurrence = Msg::new()
        .fixed64(20)
        .packed(1, &[1, 2, 3])
        .str(4, "override doc")
        .int(5, 6)
        .msg(6, &Msg::new().int(1, 1).str(3, "diagnostic"))
        .fixed32(21)
        .str(2, "s")
        .int(99, 5);
    let document = Msg::new()
        .str(4, "rust")
        .msg(2, &occurrence)
        .str(5, "fn main() {}")
        .int(6, 1)
        .int(2, 7) // a known field number with the wrong wire type
        .str(1, "src/main.rs")
        .fixed64(30);
    let index = Msg::new()
        .msg(1, &Msg::new().str(3, "file:///p"))
        .fixed32(9)
        .int(10, 3)
        .msg(2, &document)
        .str(11, "future field")
        .fixed64(12);
    let rows = occurrences_of(&index);
    assert_eq!(rows.len(), 1);
    assert_eq!(&*rows[0].document, "src/main.rs");
    assert_eq!(rows[0].symbol, "s");
    assert_eq!(rows[0].range, range(1, 2, 1, 3));
    let meta = metadata_from(index.0.as_slice()).unwrap();
    assert_eq!(meta.project_root, "file:///p");
    assert_eq!((meta.document_count, meta.occurrence_count), (1, 1));
}

#[test]
fn occurrences_stream_in_file_order_with_local_symbols_raw() {
    let doc_a = Msg::new()
        .msg(
            2,
            &Msg::new().packed(1, &[0, 0, 1]).str(2, "local 0").int(3, 1),
        )
        .str(1, "a.rs") // the path after the occurrences
        .msg(2, &Msg::new().packed(1, &[1, 0, 1]).str(2, "local 0"));
    let doc_b = Msg::new().str(1, "b.rs").msg(
        2,
        &Msg::new()
            .msg(8, &single(2, 0, 3))
            .str(2, "rust-analyzer cargo p 0.1.0 m/f()."),
    );
    let empty = Msg::new().str(1, "empty.rs");
    let index = Msg::new()
        .msg(2, &doc_a)
        .msg(3, &Msg::new().str(1, "ext"))
        .msg(2, &empty)
        .msg(2, &doc_b);
    let rows = occurrences_of(&index);
    let summary: Vec<_> = rows
        .iter()
        .map(|row| (&*row.document, row.symbol.as_str(), row.range.start_line))
        .collect();
    assert_eq!(
        summary,
        [
            ("a.rs", "local 0", 0),
            ("a.rs", "local 0", 1),
            ("b.rs", "rust-analyzer cargo p 0.1.0 m/f().", 2),
        ]
    );
    // A document without a relative_path has the empty path.
    let rows =
        occurrences_of(&Msg::new().msg(2, &Msg::new().msg(2, &Msg::new().packed(1, &[0, 0, 1]))));
    assert_eq!(&*rows[0].document, "");
}

#[test]
fn symbols_of_documents_and_external_symbols() {
    let info = Msg::new()
        .str(1, "p m/S#")
        .str(3, "```rust\nstruct S\n```")
        .str(3, "Docs.")
        .msg(4, &Msg::new().str(1, "p m/T#").int(3, 1))
        .msg(4, &Msg::new().str(1, "p m/U#").int(2, 1))
        .int(5, 49)
        .str(6, "S")
        .msg(7, &Msg::new().str(5, "struct S"))
        .str(8, "p m/");
    let local = Msg::new().str(1, "local 3").int(5, 61).str(6, "x");
    let document = Msg::new()
        .str(1, "a.rs")
        .msg(3, &info)
        .msg(2, &Msg::new().packed(1, &[0, 0, 1]))
        .msg(3, &local);
    let external = Msg::new()
        .str(1, "std v1 io/Read#")
        .int(5, 1234)
        .str(6, "Read");
    let index = Msg::new()
        .msg(3, &external)
        .msg(2, &document)
        .msg(3, &Msg::new().str(1, "bare"));
    let rows: Vec<_> = symbols_from(index.0.as_slice())
        .collect::<Result<_>>()
        .unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].document, None);
    assert_eq!(rows[0].symbol, "std v1 io/Read#");
    assert_eq!(kind_name(rows[0].kind), "1234");
    assert_eq!(rows[0].display_name, "Read");
    let s = &rows[1];
    assert_eq!(s.document.as_deref(), Some("a.rs"));
    assert_eq!(kind_name(s.kind), "Struct");
    assert_eq!(s.documentation, ["```rust\nstruct S\n```", "Docs."]);
    assert_eq!(s.relationship_count, 2);
    assert_eq!(s.display_name, "S");
    assert_eq!(s.enclosing_symbol, "p m/");
    assert_eq!(rows[2].symbol, "local 3");
    assert_eq!(kind_name(rows[2].kind), "Variable");
    assert!(rows[2].documentation.is_empty());
    let bare = &rows[3];
    assert_eq!(bare.document, None);
    assert_eq!(kind_name(bare.kind), "UnspecifiedKind");
    assert_eq!(
        (bare.display_name.as_str(), bare.enclosing_symbol.as_str()),
        ("", "")
    );
    assert_eq!(bare.relationship_count, 0);
}

#[test]
fn metadata_and_counts() {
    let tool = Msg::new()
        .str(1, "rust-analyzer")
        .str(2, "1.0")
        .str(3, "scip")
        .str(3, ".");
    let meta = Msg::new()
        .int(1, 0)
        .msg(2, &tool)
        .str(3, "file:///repo/")
        .int(4, 1);
    let doc = |n: usize| {
        let mut doc = Msg::new().str(1, "f.rs");
        for _ in 0..n {
            doc = doc.msg(2, &Msg::new().packed(1, &[0, 0, 1]));
        }
        doc.msg(3, &Msg::new().str(1, "s"))
    };
    let index = Msg::new()
        .msg(1, &meta)
        .msg(2, &doc(3))
        .msg(2, &doc(0))
        .msg(2, &doc(2))
        .msg(3, &Msg::new().str(1, "e1"))
        .msg(3, &Msg::new().str(1, "e2"));
    let row = metadata_from(index.0.as_slice()).unwrap();
    assert_eq!(
        row,
        MetadataRow {
            version: 0,
            tool_name: "rust-analyzer".into(),
            tool_version: "1.0".into(),
            tool_arguments: vec!["scip".into(), ".".into()],
            project_root: "file:///repo/".into(),
            text_document_encoding: 1,
            document_count: 3,
            occurrence_count: 5,
            external_symbol_count: 2,
        }
    );
    assert_eq!(text_encoding_name(row.text_document_encoding), "UTF8");
    assert_eq!(text_encoding_name(0), "UnspecifiedTextEncoding");
    assert_eq!(text_encoding_name(7), "7");
    // A metadata field split in two merges; an empty index has default metadata.
    let split = Msg::new()
        .msg(1, &Msg::new().msg(2, &Msg::new().str(1, "t").str(3, "a")))
        .msg(1, &Msg::new().msg(2, &Msg::new().str(3, "b")).int(4, 2));
    let row = metadata_from(split.0.as_slice()).unwrap();
    assert_eq!(row.tool_name, "t");
    assert_eq!(row.tool_arguments, ["a", "b"]);
    assert_eq!(row.text_document_encoding, 2);
    assert_eq!(metadata_from(&[][..]).unwrap(), MetadataRow::default());
}

#[test]
fn kind_names_are_sorted_and_complete() {
    assert!(KINDS.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert_eq!(KINDS.len(), 86);
    assert_eq!(kind_name(17), "Function");
    assert_eq!(kind_name(86), "Concept");
    assert_eq!(kind_name(83), "83");
}

#[test]
fn truncated_and_invalid_input_are_errors() {
    let index = Msg::new().msg(2, &Msg::new().str(1, "a.rs"));
    let truncated = &index.0[..index.0.len() - 1];
    assert!(occurrences_from(truncated).next().unwrap().is_err());
    assert!(metadata_from(truncated).is_err());
    // An unterminated varint at the top level.
    assert!(metadata_from(&[0x80u8][..]).is_err());
    // A group (wire type 3) never occurs in proto3.
    assert!(metadata_from(&[0x0bu8][..]).is_err());
    // Invalid UTF-8 in a string field.
    let bad = Msg::new().msg(2, &Msg::new().bytes(1, &[0xff, 0xfe]));
    assert!(occurrences_from(bad.0.as_slice()).next().unwrap().is_err());
    // The iterator stops after an error.
    let mut rows = occurrences_from(bad.0.as_slice());
    assert!(rows.next().unwrap().is_err());
    assert!(rows.next().is_none());
}

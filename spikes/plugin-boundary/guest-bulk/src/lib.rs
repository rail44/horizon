//! The same list built three ways, to measure what handing the document's
//! writes over together saves over a call for each
//! (`docs/extension-boundary-design.md`, "By what is wanted").
//!
//! A row is fifty-five writes, the number the board's list makes of one
//! (55,049 calls for a thousand rows): eleven elements, five texts,
//! twelve attributes, eleven classes and sixteen appends.
#![cfg(target_family = "wasm")]

wit_bindgen::generate!({
    path: "wit",
    world: "guest",
});

use spike::bulk::typed::Op;

/// What a row is made of, said to whatever takes the writes. `make`
/// answers with the number of the node made.
trait Writes {
    fn element(&mut self, tag: &str) -> u64;
    fn text(&mut self, text: &str) -> u64;
    fn attribute(&mut self, node: u64, name: &str, value: &str);
    fn class(&mut self, node: u64, class: &str);
    fn append(&mut self, parent: u64, child: u64);
}

/// One row under `list`: fifty-five writes.
fn row(to: &mut impl Writes, list: u64, index: u32) {
    let row = to.element("div");
    to.class(row, "row--1a2b3c4d");
    to.attribute(row, "data-task", &index.to_string());
    to.attribute(row, "tabindex", "0");
    // Five cells, each an element with a class, an attribute and a text
    // in a span of its own.
    let cells = ["gutter", "title", "status", "owner", "tail"];
    for (at, name) in cells.into_iter().enumerate() {
        let cell = to.element("div");
        to.class(cell, name);
        to.attribute(cell, "data-cell", name);
        let span = to.element("span");
        to.class(span, "text--5e6f7a8b");
        to.attribute(span, "style", "flex: 1; min-width: 0;");
        let words = to.text(if at == 1 { "a task's title, of some length" } else { "words" });
        to.append(span, words);
        to.append(cell, span);
        to.append(row, cell);
    }
    to.append(list, row);
}

fn build(to: &mut impl Writes, rows: u32) {
    let list = to.element("div");
    to.class(list, "list--9c0d1e2f");
    for index in 0..rows {
        row(to, list, index);
    }
}

/// A call for each write: the host numbers the nodes.
struct Each;

impl Writes for Each {
    fn element(&mut self, tag: &str) -> u64 {
        spike::bulk::each::create_element(tag)
    }
    fn text(&mut self, text: &str) -> u64 {
        spike::bulk::each::create_text(text)
    }
    fn attribute(&mut self, node: u64, name: &str, value: &str) {
        spike::bulk::each::set_attribute(node, name, value);
    }
    fn class(&mut self, node: u64, class: &str) {
        spike::bulk::each::add_class(node, class);
    }
    fn append(&mut self, parent: u64, child: u64) {
        spike::bulk::each::append(parent, child);
    }
}

/// The writes kept as typed values: the guest numbers the nodes.
#[derive(Default)]
struct Typed {
    ops: Vec<Op>,
    made: u64,
}

impl Writes for Typed {
    fn element(&mut self, tag: &str) -> u64 {
        self.made += 1;
        self.ops.push(Op::CreateElement((self.made, tag.to_string())));
        self.made
    }
    fn text(&mut self, text: &str) -> u64 {
        self.made += 1;
        self.ops.push(Op::CreateText((self.made, text.to_string())));
        self.made
    }
    fn attribute(&mut self, node: u64, name: &str, value: &str) {
        self.ops
            .push(Op::SetAttribute((node, name.to_string(), value.to_string())));
    }
    fn class(&mut self, node: u64, class: &str) {
        self.ops.push(Op::AddClass((node, class.to_string())));
    }
    fn append(&mut self, parent: u64, child: u64) {
        self.ops.push(Op::Append((parent, child)));
    }
}

/// The writes kept as bytes: a byte for which write, numbers as eight
/// bytes, a text as its length in four bytes and its bytes.
#[derive(Default)]
struct Packed {
    bytes: Vec<u8>,
    made: u64,
}

impl Packed {
    fn number(&mut self, number: u64) {
        self.bytes.extend_from_slice(&number.to_le_bytes());
    }
    fn words(&mut self, text: &str) {
        self.bytes
            .extend_from_slice(&(text.len() as u32).to_le_bytes());
        self.bytes.extend_from_slice(text.as_bytes());
    }
}

impl Writes for Packed {
    fn element(&mut self, tag: &str) -> u64 {
        self.made += 1;
        self.bytes.push(0);
        self.number(self.made);
        self.words(tag);
        self.made
    }
    fn text(&mut self, text: &str) -> u64 {
        self.made += 1;
        self.bytes.push(1);
        self.number(self.made);
        self.words(text);
        self.made
    }
    fn attribute(&mut self, node: u64, name: &str, value: &str) {
        self.bytes.push(2);
        self.number(node);
        self.words(name);
        self.words(value);
    }
    fn class(&mut self, node: u64, class: &str) {
        self.bytes.push(3);
        self.number(node);
        self.words(class);
    }
    fn append(&mut self, parent: u64, child: u64) {
        self.bytes.push(4);
        self.number(parent);
        self.number(child);
    }
}

struct Guest_;

impl Guest for Guest_ {
    fn build_each(rows: u32) {
        build(&mut Each, rows);
    }

    fn build_typed(rows: u32) {
        let mut typed = Typed::default();
        build(&mut typed, rows);
        spike::bulk::typed::apply(&typed.ops);
    }

    fn build_packed(rows: u32) {
        let mut packed = Packed::default();
        build(&mut packed, rows);
        spike::bulk::packed::apply(&packed.bytes);
    }

    fn nothing() {}
}

export!(Guest_);

//! Times a guest (`guest-bulk`) building the same list three ways, the
//! host doing the same work for each: a call for each write, the writes
//! together as typed values, the writes together as bytes.
//!
//!   host-bulk <guest_bulk.wasm> [rows]
//!
//! The host's document is a list of nodes of its own, not Blitz's: what is
//! compared is what crossing costs, and the same is done on arrival each
//! way.

use std::time::{Duration, Instant};

use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../guest-bulk/wit",
    world: "guest",
});

use spike::bulk::typed::Op;

/// A node of the host's document.
enum Node {
    Element {
        tag: String,
        attributes: Vec<(String, String)>,
        classes: Vec<String>,
        children: Vec<u64>,
    },
    Text(String),
}

/// The document: a node's number is its place, from one.
#[derive(Default)]
struct Document {
    nodes: Vec<Node>,
}

impl Document {
    fn element(&mut self, tag: String) -> u64 {
        self.nodes.push(Node::Element {
            tag,
            attributes: Vec::new(),
            classes: Vec::new(),
            children: Vec::new(),
        });
        self.nodes.len() as u64
    }

    fn text(&mut self, text: String) -> u64 {
        self.nodes.push(Node::Text(text));
        self.nodes.len() as u64
    }

    fn attribute(&mut self, node: u64, name: String, value: String) {
        if let Some(Node::Element { attributes, .. }) = self.nodes.get_mut(node as usize - 1) {
            attributes.push((name, value));
        }
    }

    fn class(&mut self, node: u64, class: String) {
        if let Some(Node::Element { classes, .. }) = self.nodes.get_mut(node as usize - 1) {
            classes.push(class);
        }
    }

    fn append(&mut self, parent: u64, child: u64) {
        if let Some(Node::Element { children, .. }) = self.nodes.get_mut(parent as usize - 1) {
            children.push(child);
        }
    }

    /// Something of everything in it, so that none of it is optimized
    /// away: how many nodes, and how long their words are.
    fn weight(&self) -> (usize, usize) {
        let words = (self.nodes.iter())
            .map(|node| match node {
                Node::Text(text) => text.len(),
                Node::Element {
                    tag,
                    attributes,
                    classes,
                    children,
                } => {
                    tag.len()
                        + attributes.iter().map(|(n, v)| n.len() + v.len()).sum::<usize>()
                        + classes.iter().map(String::len).sum::<usize>()
                        + children.len()
                }
            })
            .sum();
        (self.nodes.len(), words)
    }
}

struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    document: Document,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl spike::bulk::each::Host for State {
    fn create_element(&mut self, tag: String) -> u64 {
        self.document.element(tag)
    }
    fn create_text(&mut self, text: String) -> u64 {
        self.document.text(text)
    }
    fn set_attribute(&mut self, node: u64, name: String, value: String) {
        self.document.attribute(node, name, value);
    }
    fn add_class(&mut self, node: u64, class: String) {
        self.document.class(node, class);
    }
    fn append(&mut self, parent: u64, child: u64) {
        self.document.append(parent, child);
    }
}

impl spike::bulk::typed::Host for State {
    fn apply(&mut self, ops: Vec<Op>) {
        let document = &mut self.document;
        for op in ops {
            match op {
                Op::CreateElement((_, tag)) => _ = document.element(tag),
                Op::CreateText((_, text)) => _ = document.text(text),
                Op::SetAttribute((node, name, value)) => document.attribute(node, name, value),
                Op::AddClass((node, class)) => document.class(node, class),
                Op::Append((parent, child)) => document.append(parent, child),
            }
        }
    }
}

/// Reads the writes out of the bytes they were packed into.
struct Unpacking<'a>(&'a [u8]);

impl Unpacking<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (first, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(*first)
    }
    fn number(&mut self) -> u64 {
        let (number, rest) = self.0.split_at(8);
        self.0 = rest;
        u64::from_le_bytes(number.try_into().unwrap())
    }
    fn words(&mut self) -> String {
        let (length, rest) = self.0.split_at(4);
        let length = u32::from_le_bytes(length.try_into().unwrap()) as usize;
        let (words, rest) = rest.split_at(length);
        self.0 = rest;
        // (Held to be text as a typed value is: checked.)
        std::str::from_utf8(words).unwrap().to_string()
    }
}

impl spike::bulk::packed::Host for State {
    fn apply(&mut self, ops: Vec<u8>) {
        let document = &mut self.document;
        let mut packed = Unpacking(&ops);
        while let Some(which) = packed.byte() {
            match which {
                0 => {
                    packed.number();
                    document.element(packed.words());
                }
                1 => {
                    packed.number();
                    document.text(packed.words());
                }
                2 => {
                    let node = packed.number();
                    let name = packed.words();
                    document.attribute(node, name, packed.words());
                }
                3 => {
                    let node = packed.number();
                    document.class(node, packed.words());
                }
                _ => {
                    let parent = packed.number();
                    document.append(parent, packed.number());
                }
            }
        }
    }
}

/// The least and the middle of `times`.
fn least_and_middle(mut times: Vec<Duration>) -> (Duration, Duration) {
    times.sort();
    (times[0], times[times.len() / 2])
}

fn main() -> wasmtime::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let path = arguments.next().expect("the guest's component");
    let rows: u32 = (arguments.next()).map_or(1000, |rows| rows.parse().expect("how many rows"));

    let engine = Engine::new(&Config::new())?;
    let component = Component::from_file(&engine, &path)?;
    let mut linker = Linker::<State>::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    Guest::add_to_linker::<_, wasmtime::component::HasSelf<State>>(&mut linker, |state| state)?;
    let mut store = Store::new(
        &engine,
        State {
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            document: Document::default(),
        },
    );
    let guest = Guest::instantiate(&mut store, &component, &linker)?;

    type Build = fn(&Guest, &mut Store<State>, u32) -> wasmtime::Result<()>;
    let ways: [(&str, Build); 3] = [
        ("a call for each write", |guest, store, rows| {
            guest.call_build_each(store, rows)
        }),
        ("together, typed", |guest, store, rows| {
            guest.call_build_typed(store, rows)
        }),
        ("together, bytes", |guest, store, rows| {
            guest.call_build_packed(store, rows)
        }),
    ];
    println!("{rows} rows, fifty-five writes each");
    let mut weights = Vec::new();
    for (name, build) in ways {
        let mut times = Vec::new();
        for _ in 0..40 {
            store.data_mut().document = Document::default();
            let started = Instant::now();
            build(&guest, &mut store, rows)?;
            times.push(started.elapsed());
        }
        let weight = store.data().document.weight();
        weights.push(weight);
        let (least, middle) = least_and_middle(times);
        println!("{name:>24}: least {least:>10.3?}  middle {middle:>10.3?}  {weight:?}");
    }
    assert!(weights.windows(2).all(|pair| pair[0] == pair[1]), "{weights:?}");

    // What calling the guest costs, with nothing done.
    let calls = 200_000u32;
    let started = Instant::now();
    for _ in 0..calls {
        guest.call_nothing(&mut store)?;
    }
    println!("{:>24}: {:?} a call", "a call that does nothing", started.elapsed() / calls);
    Ok(())
}

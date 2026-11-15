//! A plugin described once in Rust, read as a host reads a plugin: the
//! component's type, the manifest in its custom section, and calls into
//! it. The guest is `guest-macro`, built for wasm32-wasip2 first
//! (`check-macro.sh`).

use std::path::PathBuf;

use wasmtime::component::types::{ComponentItem, Type};
use wasmtime::component::{
    Component, HasSelf, Instance, Linker, Resource, ResourceAny, ResourceTable, Val,
};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../plugin-macro/wit",
    world: "imports",
    imports: { default: trappable },
    with: { "spike:host/files.file": File },
});

/// A file, as the host hands one to a plugin.
pub struct File(String);

struct State {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl spike::host::files::Host for State {}

impl spike::host::files::HostFile for State {
    fn path(&mut self, file: Resource<File>) -> wasmtime::Result<String> {
        Ok(self.table.get(&file)?.0.clone())
    }

    fn drop(&mut self, file: Resource<File>) -> wasmtime::Result<()> {
        self.table.delete(file)?;
        Ok(())
    }
}

fn guest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/wasm32-wasip2/debug/guest_macro.wasm")
}

/// The custom section `name` of the wasm at `guest()`, as text.
fn section(name: &str) -> String {
    let bytes = std::fs::read(guest()).expect("the guest: check-macro.sh builds it");
    for part in wasmparser::Parser::new(0).parse_all(&bytes) {
        if let wasmparser::Payload::CustomSection(section) = part.expect("wasm")
            && section.name() == name
        {
            return String::from_utf8(section.data().to_vec()).expect("text");
        }
    }
    panic!("no section `{name}`");
}

/// A type as WIT would write it, as far as the host can tell.
fn written(ty: &Type) -> String {
    match ty {
        Type::String => "string".to_string(),
        Type::U32 => "u32".to_string(),
        Type::U64 => "u64".to_string(),
        Type::Option(inner) => format!("option<{}>", written(&inner.ty())),
        Type::Record(record) => {
            let fields: Vec<String> = (record.fields())
                .map(|field| format!("{}: {}", field.name, written(&field.ty)))
                .collect();
            format!("record {{ {} }}", fields.join(", "))
        }
        Type::Own(_) => "own<resource>".to_string(),
        other => format!("{other:?}"),
    }
}

/// A function's signature as WIT would write it.
fn signature(function: &wasmtime::component::types::ComponentFunc) -> String {
    let parameters: Vec<String> = (function.params())
        .map(|(name, ty)| format!("{name}: {}", written(&ty)))
        .collect();
    let results: Vec<String> = function.results().map(|ty| written(&ty)).collect();
    match results.as_slice() {
        [] => format!("func({})", parameters.join(", ")),
        [result] => format!("func({}) -> {result}", parameters.join(", ")),
        _ => unreachable!("one result at most"),
    }
}

#[test]
fn the_component_s_type_is_the_one_a_hand_written_world_would_give() {
    let engine = Engine::default();
    let component = Component::from_file(&engine, guest()).expect("the guest");
    let shape = component.component_type();
    // The host's file, as the component imports it.
    let file = (shape.imports(&engine))
        .find_map(|(name, item)| match item.ty {
            ComponentItem::ComponentInstance(instance) if name.starts_with("spike:host/files") => {
                (instance.exports(&engine)).find_map(|(name, item)| match item.ty {
                    ComponentItem::Resource(file) if name == "file" => Some(file),
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("the host's file imported");
    let mut world = Vec::new();
    let mut commands = Vec::new();
    for (name, item) in shape.exports(&engine) {
        match item.ty {
            ComponentItem::ComponentFunc(function) => {
                if name == "create" {
                    // What `create` takes is the host's own file.
                    let (_, parameter) = function.params().next().expect("a parameter");
                    assert_eq!(parameter, Type::Own(file));
                }
                world.push(format!("{name}: {}", signature(&function)));
            }
            ComponentItem::ComponentInstance(instance) if name == "commands" => {
                for (name, item) in instance.exports(&engine) {
                    if let ComponentItem::ComponentFunc(function) = item.ty {
                        commands.push(format!("{name}: {}", signature(&function)));
                    }
                }
            }
            _ => {}
        }
    }
    world.sort();
    commands.sort();
    let state = "record { path: option<string>, line: u32 }";
    assert_eq!(
        world,
        [
            "create: func(file: own<resource>) -> ".to_string() + state,
            format!("mount: func(state: option<{state}>) -> u64"),
            format!("state: func() -> {state}"),
        ]
    );
    assert_eq!(
        commands,
        ["back: func()", "forget: func()", "go-to-line: func(line: u32)"]
    );
}

#[test]
fn the_manifest_is_in_the_wasm_and_names_the_commands_the_component_has() {
    let manifest: toml::Table = section("horizon:manifest").parse().expect("a manifest");
    assert_eq!(manifest["plugin"]["name"].as_str(), Some("notes"));
    assert_eq!(manifest["plugin"]["title"].as_str(), Some("Notes"));
    let commands = manifest["commands"].as_table().expect("the commands");
    let mut named: Vec<&str> = commands.keys().map(String::as_str).collect();
    named.sort();
    assert_eq!(named, ["back", "forget", "go-to-line"]);
    assert_eq!(commands["back"]["title"].as_str(), Some("Back"));
    assert_eq!(commands["forget"]["destructive"].as_bool(), Some(true));
    assert!(commands["back"].get("destructive").is_none());
    let extensions = manifest["create"]["file"]["extensions"].as_array().expect("a list");
    assert_eq!(extensions.len(), 2);
    assert_eq!(manifest["access"]["read"][0].as_str(), Some("/"));
}

/// Prints the world the macro wrote: `cargo test -- --nocapture`.
#[test]
fn the_world_the_macro_wrote() {
    let world = section("horizon:wit");
    println!("{world}");
    assert!(world.contains("export create: func(file: file) -> state;"), "{world}");
    assert!(world.contains("go-to-line: func(line: u32);"), "{world}");
}

/// The function `name` the instance exports, inside `within` if given.
fn function(
    instance: &Instance,
    store: &mut Store<State>,
    within: Option<&str>,
    name: &str,
) -> wasmtime::component::Func {
    let within =
        within.map(|within| instance.get_export_index(&mut *store, None, within).expect("there"));
    let index = (instance.get_export_index(&mut *store, within.as_ref(), name)).expect("there");
    instance.get_func(&mut *store, index).expect("a function")
}

#[test]
fn the_plugin_is_called_as_any_plugin_is() {
    let engine = Engine::default();
    let component = Component::from_file(&engine, guest()).expect("the guest");
    let mut linker = Linker::<State>::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).expect("wasi");
    Imports::add_to_linker::<_, HasSelf<State>>(&mut linker, |state| state).expect("the host");
    let mut store = Store::new(
        &engine,
        State {
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        },
    );
    let instance = linker.instantiate(&mut store, &component).expect("the plugin");
    // (A file the host hands over, for `create`.)
    let file = store.data_mut().table.push(File("/notes/a.md".to_string())).unwrap();
    let file = ResourceAny::try_from_resource(file, &mut store).unwrap();
    let mut call = |within: Option<&str>, name: &str, given: &[Val], results: usize| {
        let function = function(&instance, &mut store, within, name);
        let mut answer = vec![Val::Bool(false); results];
        function.call(&mut store, given, &mut answer).expect("the call");
        answer.into_iter().next()
    };
    let field = |state: &Val, name: &str| -> Val {
        let Val::Record(fields) = state else {
            panic!("not a record: {state:?}");
        };
        let (_, value) = fields.iter().find(|(field, _)| field == name).expect("the field");
        value.clone()
    };

    // A new pane's state, from the file.
    let made = call(None, "create", &[Val::Resource(file)], 1).expect("a state");
    assert_eq!(
        field(&made, "path"),
        Val::Option(Some(Box::new(Val::String("/notes/a.md".to_string()))))
    );

    // The view on that state, and the plugin's commands.
    let root = call(None, "mount", &[Val::Option(Some(Box::new(made)))], 1);
    assert_eq!(root, Some(Val::U64(7)));
    call(Some("commands"), "go-to-line", &[Val::U32(12)], 0);
    let now = call(None, "state", &[], 1).expect("a state");
    assert_eq!(field(&now, "line"), Val::U32(12));
    call(Some("commands"), "forget", &[], 0);
    let now = call(None, "state", &[], 1).expect("a state");
    assert_eq!(field(&now, "path"), Val::Option(None));
}

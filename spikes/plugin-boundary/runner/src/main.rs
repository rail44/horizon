//! Runs a command component with the arguments after its path, lending it
//! `/usr/share/fonts` to read. Prints how long compiling it took.

use std::time::Instant;

use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::p2::bindings::sync::Command;
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

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

fn main() -> wasmtime::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let path = arguments.next().expect("a component's path");
    let passed: Vec<String> = std::iter::once(path.clone()).chain(arguments).collect();

    let engine = Engine::new(&Config::new())?;
    let started = Instant::now();
    let component = Component::from_file(&engine, &path)?;
    let compiled = started.elapsed();

    let mut linker = Linker::<State>::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    let mut wasi = WasiCtxBuilder::new();
    wasi.inherit_stdout().inherit_stderr().args(&passed);
    wasi.preopened_dir("/usr/share/fonts", "/usr/share/fonts", FsPerms::ReadOnly)?;
    let mut store = Store::new(
        &engine,
        State {
            wasi: wasi.build(),
            table: ResourceTable::new(),
        },
    );
    let started = Instant::now();
    let command = Command::instantiate(&mut store, &component, &linker)?;
    let instantiated = started.elapsed();
    let ran = command.wasi_cli_run().call_run(&mut store)?;
    println!("compile={compiled:?} instantiate={instantiated:?} ok={}", ran.is_ok());
    Ok(())
}

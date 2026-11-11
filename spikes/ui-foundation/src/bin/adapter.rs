//! Says which graphics adapters wgpu finds here, and which one an
//! offscreen renderer would be given: `cargo run --release --bin adapter`.

fn main() {
    // The instance the offscreen renderer makes (`wgpu_context`).
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::from_env().unwrap_or_default(),
        flags: wgpu::InstanceFlags::from_build_config().with_env(),
        backend_options: wgpu::BackendOptions::from_env_or_default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    });
    for adapter in pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())) {
        let info = adapter.get_info();
        println!(
            "found: {} | {:?} | {:?} | driver {} {}",
            info.name, info.device_type, info.backend, info.driver, info.driver_info
        );
    }
    let chosen =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()));
    match chosen {
        Ok(adapter) => {
            let info = adapter.get_info();
            println!(
                "given by default: {} | {:?} | {:?}",
                info.name, info.device_type, info.backend
            );
        }
        Err(error) => println!("no adapter: {error}"),
    }
}

//! The renderers timed, each drawing off screen into pixels read back.

use anyrender::ImageRenderer;
use anyrender_vello::VelloImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use anyrender_vello_gpu::wgpu::{CommandEncoderDescriptor, Features, TextureFormat, TextureUsages};
use anyrender_vello_gpu::{ExternalTextures, ImageCache, ImageManager, VelloGpuScenePainter};
use vello_gpu::{
    RenderOptions, RenderSettings, RenderSize, RenderTarget, RenderTargetConfig, Renderer,
    Resources, Scene,
};
use wgpu_context::{BufferRenderer, BufferRendererConfig, WGPUContext};

use crate::screen::Screen;

pub trait Target {
    fn new(width: u32, height: u32) -> Self;
    /// Draws `screen` and reads the pixels back.
    fn frame(&mut self, screen: &Screen, pixels: &mut Vec<u8>);
}

/// `vello`, the compute-shader renderer: what Horizon's window has.
pub struct Compute(VelloImageRenderer);

impl Target for Compute {
    fn new(width: u32, height: u32) -> Self {
        Self(VelloImageRenderer::new(width, height))
    }

    fn frame(&mut self, screen: &Screen, pixels: &mut Vec<u8>) {
        self.0.render_to_vec(|scene| screen.paint(scene), pixels);
    }
}

/// `vello_cpu`, with its glyph cache or without.
pub struct Cpu<const CACHED: bool>(VelloCpuImageRenderer);

impl<const CACHED: bool> Target for Cpu<CACHED> {
    fn new(width: u32, height: u32) -> Self {
        let mut renderer = VelloCpuImageRenderer::new(width, height);
        renderer.set_glyph_caching(CACHED);
        Self(renderer)
    }

    fn frame(&mut self, screen: &Screen, pixels: &mut Vec<u8>) {
        self.0.render_to_vec(|scene| screen.paint(scene), pixels);
    }
}

/// `vello_gpu`, with its glyph cache or without. `anyrender_vello_gpu` 0.12
/// draws to a window only, so this puts its scene painter over a texture
/// the way its window renderer puts it over a surface.
pub struct Gpu<const CACHED: bool> {
    buffer: BufferRenderer,
    renderer: Renderer,
    resources: Resources,
    depth: anyrender_vello_gpu::wgpu::TextureView,
    scene: Scene,
    images: ImageCache,
    textures: ExternalTextures,
}

/// The adapter the GPU renderers draw with, as wgpu names it.
pub fn adapter() -> String {
    let gpu = <Gpu<false> as Target>::new(16, 16);
    let info = gpu.buffer.device_handle.adapter.get_info();
    format!("{} ({:?}, {:?})", info.name, info.device_type, info.backend)
}

impl<const CACHED: bool> Target for Gpu<CACHED> {
    fn new(width: u32, height: u32) -> Self {
        let features = Features::CLEAR_TEXTURE | Features::PIPELINE_CACHE;
        let mut context = WGPUContext::with_features_and_limits(Some(features), None);
        let buffer = pollster::block_on(context.create_buffer_renderer(BufferRendererConfig {
            width,
            height,
            usage: TextureUsages::RENDER_ATTACHMENT,
        }))
        .expect("no device to draw with");
        let settings = RenderSettings::default();
        let size = RenderSize {
            width: width as u16,
            height: height as u16,
        };
        let (renderer, resources) = Renderer::new_with(
            buffer.device(),
            buffer.queue(),
            &RenderTargetConfig {
                // The format of `BufferRenderer`'s texture.
                format: TextureFormat::Rgba8Unorm,
                width: size.width,
                height: size.height,
            },
            settings.clone(),
        );
        Self {
            depth: renderer.create_depth_texture_view(&size),
            scene: Scene::new_with(size.width, size.height, settings.level()),
            buffer,
            renderer,
            resources,
            images: ImageCache::default(),
            textures: ExternalTextures::default(),
        }
    }

    fn frame(&mut self, screen: &Screen, pixels: &mut Vec<u8>) {
        let (device, queue) = (self.buffer.device(), self.buffer.queue());
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor::default());
        let images = ImageManager::new(
            &mut self.resources,
            device,
            queue,
            &mut self.images,
            &mut self.textures,
        );
        let mut painter =
            VelloGpuScenePainter::new(&mut self.scene, images, &self.buffer.device_handle)
                .with_glyph_caching(CACHED);
        screen.paint(&mut painter);
        drop(painter);
        let view = self.buffer.target_texture_view();
        self.renderer
            .render(
                &self.scene,
                &mut self.resources,
                &mut encoder,
                RenderTarget {
                    view: &view,
                    depth_view: Some(&self.depth),
                },
                self.textures.bindings(),
                RenderOptions::default(),
            )
            .expect("the scene drawn");
        ImageManager::new(
            &mut self.resources,
            device,
            queue,
            &mut self.images,
            &mut self.textures,
        )
        .finish_frame();
        queue.submit([encoder.finish()]);
        let size = self.buffer.size();
        pixels.resize((size.width * size.height * 4) as usize, 0);
        self.buffer.copy_texture_to_buffer(pixels);
        self.scene.reset();
    }
}

//! The rows a terminal has drawn, kept as pixels in a texture of its own on
//! the window's device, and a frame painted from them
//! (`docs/terminal-row-texture-design.md` §3, §4).
//!
//! The texture is registered with the renderer the page is painted with
//! (`RenderContext::try_register_custom_resource`); a frame is one fill per
//! row, drawing the row's slot of it. Rows not held yet are drawn with
//! Vello into a scratch texture and copied into their slots: Vello writes
//! the whole of the texture it renders to, and wgpu copies nothing within
//! one texture.
//!
//! Drawing rows into the store is a render of its own, which costs about
//! the same for one row as for many. A few rows new to a frame are
//! painted into the frame itself instead, and put in the store when the
//! page is drawn again with nothing changed (§4.4).

use std::cell::RefCell;
use std::ops::Range;
use std::rc::{Rc, Weak};

use anyrender::{AntiAliasing, Paint, PaintScene, RenderContext, ResourceId};
use anyrender_vello::{DeviceHandle, VelloScenePainter};
use horizon_terminal_core::TerminalLine;
use kurbo::{Affine, Rect};
use peniko::{Fill, ImageBrush, ImageQuality, ImageSampler};
use vello::wgpu;

use super::slots::{Layout, Placed, Slots, hash_of};
use super::{Pen, Shaped, paint_row};

/// What a slot holds a row's pixels from: its content and the columns of
/// the selection on it, and the shaping of the row it was drawn from.
struct Held {
    line: TerminalLine,
    selected: Option<Range<usize>>,
    shaping: u64,
}

/// What every row of the store is drawn with: when it changes, no row
/// held holds.
#[derive(Clone, PartialEq)]
pub(super) struct Ground {
    /// Which of the looks given (font, colors, size).
    pub styled: u64,
    /// The colors the program has set over the look's.
    pub set: Vec<(u16, [u8; 3])>,
    /// The display's scale, a cell's width, and how far into a device pixel
    /// the box starts, as their bits.
    pub scale: u64,
    pub cell: u64,
    pub inset: (u64, u64),
}

/// How many cells with something drawn in them (not blank) a frame paints
/// itself rather than putting them in the store. Painted that way, they are
/// drawn again with the page each time until they are put in the store, so
/// a frame does so only while that costs less than one render of the store:
/// measured in a window at 48000 cells, the render took about 1 ms, and
/// rows of text took about as long at a little over 1000 cells
/// (`docs/terminal-row-texture-design.md` §4.4).
const PAINTED_IN_THE_FRAME: usize = 1024;

/// What a row of a frame is found by in the store: the hash of what it is
/// drawn from, and whether what a slot holds is it.
fn key<'a>(row: &Wanted<'a>) -> (u64, impl Fn(&Held) -> bool + 'a) {
    let shaped = row.shaped;
    let hash = hash_of(&(shaped.hash, &row.selected));
    let selected = row.selected.clone();
    let same = move |held: &Held| {
        held.selected == selected && (held.shaping == shaped.shaping || held.line == shaped.line)
    };
    (hash, same)
}

/// A row of a frame as the store paints it.
pub(super) struct Wanted<'a> {
    pub shaped: &'a Shaped,
    pub selected: Option<Range<usize>>,
    /// Its top in the grid, in device pixels.
    pub top: f64,
}

pub(super) struct RowStore {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Rc<Shared>,
    layout: Layout,
    texture: wgpu::Texture,
    scratch: wgpu::Texture,
    /// How far apart rows are drawn in the scratch texture, and how many
    /// it takes at once.
    pitch: u32,
    batch: u32,
    resource: ResourceId,
    slots: Slots<Held>,
    ground: Option<Ground>,
}

impl RowStore {
    /// A store laid out as `layout` on `handle`'s device, registered with
    /// the renderer behind `context`. `None` when the renderer takes no
    /// texture.
    pub(super) fn new(
        context: &mut dyn RenderContext,
        handle: &DeviceHandle,
        layout: Layout,
    ) -> Option<Self> {
        let renderer = shared_renderer(&handle.device)?;
        let (width, height) = layout.size();
        let texture = texture(
            &handle.device,
            "terminal rows",
            (width, height),
            // Copied into from the scratch texture, and by the page's
            // renderer into its image atlas.
            wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        );
        // A row drawn into any place of the scratch texture lies the same
        // way across Vello's tiles, so it comes out the same pixels
        // wherever it was drawn.
        let pitch = layout.row_px.div_ceil(TILE) * TILE;
        let greatest = handle.device.limits().max_texture_dimension_2d;
        let batch = (greatest / pitch).min(layout.per_column).max(1);
        let scratch = texture_for_vello(&handle.device, (layout.width, batch * pitch));
        let resource = context
            .try_register_custom_resource(Box::new(texture.clone()))
            .ok()?;
        Some(Self {
            device: handle.device.clone(),
            queue: handle.queue.clone(),
            renderer,
            slots: Slots::new(layout.capacity() as usize),
            layout,
            texture,
            scratch,
            pitch,
            batch,
            resource,
            ground: None,
        })
    }

    /// Whether this store is laid out as `layout` on `handle`'s device.
    pub(super) fn is(&self, handle: &DeviceHandle, layout: &Layout) -> bool {
        self.device == handle.device && self.layout == *layout
    }

    /// Lets go of the texture the page's renderer was given.
    pub(super) fn release(self, context: &mut dyn RenderContext) {
        context.unregister_resource(self.resource);
    }

    /// Paints `rows` into `scene`, all with `pen` on `ground`, the grid
    /// `height` device pixels tall: those held from their slots, and those
    /// not held either drawn into the store first or, when there are few
    /// and `store_all` is false, painted into `scene` themselves. How many
    /// rows were painted themselves, or `None` when the rows could not be
    /// painted, and nothing was.
    pub(super) fn paint(
        &mut self,
        scene: &mut impl PaintScene,
        pen: &Pen,
        ground: &Ground,
        rows: &[Wanted<'_>],
        height: f64,
        store_all: bool,
    ) -> Option<usize> {
        if self.ground.as_ref() != Some(ground) {
            self.slots.clear();
            self.ground = Some(ground.clone());
        }
        self.slots.next_frame();
        let mut placed = Vec::with_capacity(rows.len());
        let mut missing = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            // A blank row is the ground the screen is filled with already.
            if row.shaped.blank && row.selected.is_none() {
                placed.push(None);
                continue;
            }
            let (hash, same) = key(row);
            let slot = self.slots.find(hash, same);
            if slot.is_none() {
                missing.push(index);
            }
            placed.push(slot);
        }
        let drawn = |index: &usize| {
            let line = &rows[*index].shaped.line;
            let glyphs = line.text.chars().filter(|c| !c.is_whitespace());
            glyphs.count() + line.shapes.len()
        };
        let in_the_frame =
            !store_all && missing.iter().map(drawn).sum::<usize>() <= PAINTED_IN_THE_FRAME;
        if in_the_frame {
            self.compose(scene, pen, rows, &placed, height);
            scene.push_clip_layer(
                Fill::NonZero,
                Affine::IDENTITY,
                &Rect::new(0., 0., pen.width, height),
            );
            for index in &missing {
                let row = &rows[*index];
                paint_row(scene, pen, row.shaped, row.selected.clone(), row.top);
            }
            scene.pop_layer();
            return Some(missing.len());
        }
        let mut fresh = Vec::with_capacity(missing.len());
        for index in missing {
            let row = &rows[index];
            let (hash, same) = key(row);
            let make = || Held {
                line: row.shaped.line.clone(),
                selected: row.selected.clone(),
                shaping: row.shaped.shaping,
            };
            match self.slots.place(hash, same, make) {
                // (A row alike placed just before it.)
                Some(Placed::Kept(slot)) => placed[index] = Some(slot),
                Some(Placed::Fresh(slot)) => {
                    placed[index] = Some(slot);
                    fresh.push((slot, index));
                }
                None => return None,
            }
        }
        if !fresh.is_empty() && !self.draw(pen, rows, &fresh) {
            // What the slots were said to hold was not drawn.
            self.slots.clear();
            return None;
        }
        self.compose(scene, pen, rows, &placed, height);
        Some(0)
    }

    /// Paints the rows of `rows` that `placed` gives a slot from it.
    fn compose(
        &self,
        scene: &mut impl PaintScene,
        pen: &Pen,
        rows: &[Wanted<'_>],
        placed: &[Option<usize>],
        height: f64,
    ) {
        let image = Paint::Resource(ImageBrush {
            image: self.resource,
            sampler: ImageSampler {
                // Drawn one to one, on whole device pixels.
                quality: ImageQuality::Low,
                ..ImageSampler::default()
            },
        });
        for (row, slot) in rows.iter().zip(placed) {
            let Some(slot) = *slot else {
                continue;
            };
            let (x, y) = self.layout.origin(slot);
            let strip = Rect::new(0., row.top, pen.width, row.top + pen.line)
                .intersect(Rect::new(0., 0., pen.width, height));
            if strip.is_zero_area() {
                continue;
            }
            let from_slot = Affine::translate((-(x as f64), row.top - y as f64));
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                &image,
                Some(from_slot),
                &strip,
                AntiAliasing::Enabled,
            );
        }
    }

    /// Draws the rows of `rows` that `fresh` names into the slots it names.
    fn draw(&mut self, pen: &Pen, rows: &[Wanted<'_>], fresh: &[(usize, usize)]) -> bool {
        let Layout { width, row_px, .. } = self.layout;
        let pitch = self.pitch;
        let view = self
            .scratch
            .create_view(&wgpu::TextureViewDescriptor::default());
        for batch in fresh.chunks(self.batch as usize) {
            let mut drawn = vello::Scene::new();
            {
                let mut painter = VelloScenePainter::new(&mut drawn);
                for (place, (_, index)) in batch.iter().enumerate() {
                    let row = &rows[*index];
                    let top = (place as u32 * pitch) as f64;
                    paint_row(&mut painter, pen, row.shaped, row.selected.clone(), top);
                }
            }
            let params = vello::RenderParams {
                base_color: peniko::color::palette::css::TRANSPARENT,
                width,
                height: batch.len() as u32 * pitch,
                antialiasing_method: ANTIALIASING,
            };
            let rendered = (self.renderer.renderer.borrow_mut()).render_to_texture(
                &self.device,
                &self.queue,
                &drawn,
                &view,
                &params,
            );
            if rendered.is_err() {
                return false;
            }
            let mut encoder =
                (self.device).create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("terminal rows"),
                });
            for (place, (slot, _)) in batch.iter().enumerate() {
                let (x, y) = self.layout.origin(*slot);
                encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.scratch,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: place as u32 * pitch,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x, y, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width,
                        height: row_px,
                        depth_or_array_layers: 1,
                    },
                );
            }
            self.queue.submit([encoder.finish()]);
        }
        true
    }
}

/// The side of the square tiles Vello rasterizes in.
const TILE: u32 = 16;

/// How rows are anti-aliased: as the window's renderer draws the rest of
/// the page (`anyrender_vello`'s default, which `src/application.rs` keeps).
const ANTIALIASING: vello::AaConfig = vello::AaConfig::Msaa16;

/// A Vello renderer on a device: making one compiles its pipelines, so the
/// stores on one device share one.
struct Shared {
    device: wgpu::Device,
    renderer: RefCell<vello::Renderer>,
}

thread_local! {
    /// The renderer the stores on this thread share, while any holds it.
    /// Only they hold it: what wgpu keeps is let go of as they are, not
    /// when the thread ends.
    static SHARED: RefCell<Weak<Shared>> = const { RefCell::new(Weak::new()) };
}

/// The renderer for `device`, made if no store holds one for it. `None`
/// when one cannot be made.
fn shared_renderer(device: &wgpu::Device) -> Option<Rc<Shared>> {
    SHARED.with_borrow_mut(|held| {
        if let Some(shared) = held.upgrade()
            && shared.device == *device
        {
            return Some(shared);
        }
        let options = vello::RendererOptions {
            use_cpu: false,
            antialiasing_support: vello::AaSupport {
                area: false,
                msaa8: false,
                msaa16: true,
            },
            num_init_threads: None,
            pipeline_cache: None,
        };
        let shared = Rc::new(Shared {
            device: device.clone(),
            renderer: RefCell::new(vello::Renderer::new(device, options).ok()?),
        });
        *held = Rc::downgrade(&shared);
        Some(shared)
    })
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    (width, height): (u32, u32),
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage,
        view_formats: &[],
    })
}

/// A texture Vello can render into and rows can be copied out of.
fn texture_for_vello(device: &wgpu::Device, size: (u32, u32)) -> wgpu::Texture {
    texture(
        device,
        "terminal rows drawn",
        size,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
    )
}

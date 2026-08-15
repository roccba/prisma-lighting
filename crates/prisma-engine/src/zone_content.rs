//! The per-zone compositor content (CLAUDE.md M8, docs/M8-PARTA.md §2, §3).
//!
//! [`ZoneCompositor`] is the [`FrameContent`] that replaces [`IdleContent`] once a
//! show runs. Per frame it renders **each zone's effect at that zone's own
//! `mapping_size`**, **places** each into the world framebuffer at the zone's
//! footprint (`prisma_render::ZonePlacer`), and folds the placed textures with the
//! **unchanged** M5 blend machinery (`Compositor::blend_zone`). The gather, the
//! overlap counter and the output stage downstream keep operating on the one world
//! framebuffer exactly as in M5 — this content only changes *where a zone's pixels
//! came from* (a per-`mapping_size` texture, placed), never the world coordinate.
//!
//! # Hot-path allocation: zero (M8-PARTA §2.6, guardrail)
//!
//! Everything GPU-sized is allocated **once**, at construction, from the compiled
//! layout: per-zone `mapping_size` textures and world-sized placed textures, the
//! per-zone coverage buffers, the two world-sized fold scratch textures, and the
//! per-frame cell-overlap mask buffer. Per frame the loop:
//! - writes the frame's cell mask with `queue.write_buffer` (no buffer alloc),
//! - reuses one `Vec<u32>` (`contrib`) cleared and refilled in place,
//! - folds by calling `blend_zone` directly in a ping-pong over the two scratch
//!   textures — no per-frame `Vec<ZoneComposite>` and, crucially, **no GPU buffer
//!   or texture allocation**.
//!
//! # Per-zone speed (M5 gap closed here, guardrail)
//!
//! `speed_multiplier` scales the **effect's** show-time only — a per-zone *copy* of
//! the clock snapshot with `show_ns` scaled — never the master clock, which would
//! desync everything ([`scaled_clock`]).

use prisma_core::zone::ZoneStates;
use prisma_core::{ClockSnapshot, OverlapSnapshot};
use prisma_patch::{BlendMode, Patch, PatchIndex};
use prisma_render::blend::Compositor;
use prisma_render::effect::EffectInstance;
use prisma_render::overlap::zone_cell_coverage;
use prisma_render::place::{footprint_from_coverage, Footprint, ZonePlacer};
use prisma_render::readback::create_render_target;
use prisma_render::render_loop::FrameContent;
use prisma_render::{create_cell_mask, Gpu};

/// One zone's GPU-side resources, all sized once at construction.
struct ZoneSlot {
    /// The zone's own local matrix resolution (`Zone::mapping_size`, X/Y).
    mapping: (u32, u32),
    /// Where the zone's local space lands on the world framebuffer — derived from
    /// the zone's coverage mask so placement is always a superset of coverage
    /// (M8-PARTA §2.3).
    footprint: Footprint,
    /// The effect rendered at `mapping`, once per frame.
    zone_tex: wgpu::Texture,
    /// The placed result at world resolution — the compositor's input for this zone.
    placed_tex: wgpu::Texture,
    /// Per-cell coverage mask (world cells), uploaded once.
    coverage: wgpu::Buffer,
    /// How this zone blends onto the zones below it.
    blend_mode: BlendMode,
    /// The active cell's effect, or `None` when the zone has nothing playing.
    effect: Option<EffectInstance>,
}

/// The per-zone compositor. Implements [`FrameContent`]; install it in place of
/// [`IdleContent`](crate::IdleContent) when a show runs.
pub struct ZoneCompositor {
    compositor: Compositor,
    placer: ZonePlacer,
    /// Bottom-to-top compositing order (`PatchIndex::composite_order`), once.
    order: Vec<u32>,
    slots: Vec<ZoneSlot>,
    /// This frame's per-cell overlap mask (world cells). Persistent; rewritten each
    /// frame from the snapshot with `write_buffer`.
    cell_mask: wgpu::Buffer,
    /// World-sized ping/pong scratch for the fold. Persistent.
    ping: wgpu::Texture,
    pong: wgpu::Texture,
    /// Reused each frame: contributing zones in compositing order. Cleared + refilled
    /// in place, so it allocates only on the first (or a larger) frame.
    contrib: Vec<u32>,
}

impl ZoneCompositor {
    /// Build the compositor for a compiled layout. `effects` is indexed by **zone
    /// slot** (the same coordinate as `Patch::zones` and the overlap counter); a
    /// `None` slot is a zone with no active cell. All GPU resources are allocated
    /// here — nothing below allocates GPU memory per frame.
    pub fn new(
        gpu: &Gpu,
        patch: &Patch,
        index: &PatchIndex,
        mut effects: Vec<Option<EffectInstance>>,
    ) -> ZoneCompositor {
        let world = (patch.matrix_size[0].max(1), patch.matrix_size[1].max(1));
        assert_eq!(
            effects.len(),
            index.zone_count(),
            "one effect slot per zone (None allowed)"
        );

        let coverage_masks = zone_cell_coverage(patch, index);
        let slots: Vec<ZoneSlot> = (0..index.zone_count())
            .map(|slot| {
                let zone = &patch.zones[slot];
                let mapping = (zone.mapping_size[0].max(1), zone.mapping_size[1].max(1));
                let footprint = footprint_from_coverage(&coverage_masks[slot], world.0);
                ZoneSlot {
                    mapping,
                    footprint,
                    zone_tex: create_render_target(gpu, mapping.0, mapping.1),
                    placed_tex: create_render_target(gpu, world.0, world.1),
                    coverage: create_cell_mask(gpu, &coverage_masks[slot]),
                    blend_mode: zone.blend_mode,
                    effect: effects[slot].take(),
                }
            })
            .collect();

        let cell_count = (world.0 as usize) * (world.1 as usize);
        let cell_mask = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("zone-cell-overlap-mask"),
            size: (cell_count * std::mem::size_of::<u32>()).max(4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        ZoneCompositor {
            compositor: Compositor::new(gpu),
            placer: ZonePlacer::new(gpu),
            order: index.composite_order().to_vec(),
            slots,
            cell_mask,
            ping: create_render_target(gpu, world.0, world.1),
            pong: create_render_target(gpu, world.0, world.1),
            contrib: Vec::new(),
        }
    }

    /// Replace a zone's active effect (`None` clears it). This is the structural
    /// hot-swap the frame-boundary command queue drives (M8-PARTA §3); building the
    /// `EffectInstance` happens off-thread, the swap is a pointer move. Textures and
    /// footprint are unchanged — only the content source moves.
    pub fn set_zone_effect(&mut self, slot: usize, effect: Option<EffectInstance>) {
        self.slots[slot].effect = effect;
    }
}

/// A per-zone view of the clock with only the effect's **show-time** scaled by
/// `speed` — never the master clock (guardrail, M8-PARTA §3). `frame`/`beat`/`bpm`
/// are left as-is; the built-in effects animate off `show_seconds`.
fn scaled_clock(clock: &ClockSnapshot, speed: f32) -> ClockSnapshot {
    let mut c = *clock;
    c.show_ns = (clock.show_ns as f64 * speed.max(0.0) as f64) as u64;
    c
}

/// A zone's linear-light scale for the compositor: `dimmer × color_filter`.
fn zone_scale(zones: &ZoneStates, slot: usize) -> [f32; 3] {
    let z = zones.get(slot);
    let d = z.dimmer();
    [
        d * z.color_filter[0],
        d * z.color_filter[1],
        d * z.color_filter[2],
    ]
}

fn view(tex: &wgpu::Texture) -> wgpu::TextureView {
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

impl FrameContent for ZoneCompositor {
    fn render_into(
        &mut self,
        gpu: &Gpu,
        clock: &ClockSnapshot,
        zones: &ZoneStates,
        overlap: &OverlapSnapshot,
        target: &wgpu::Texture,
    ) {
        // 1. Which zones actually contribute this frame, in compositing order. A
        //    zone contributes iff it has live state (contributes_pixels), an active
        //    effect, and a non-empty footprint. Reuses `contrib` in place.
        self.contrib.clear();
        for &slot in &self.order {
            let s = slot as usize;
            let slot_ref = &self.slots[s];
            if zones.contributes_pixels(s)
                && slot_ref.effect.is_some()
                && !slot_ref.footprint.is_empty()
            {
                self.contrib.push(slot);
            }
        }

        // 2. Render each contributing zone's effect at its OWN mapping_size (with a
        //    per-zone speed-scaled clock), then PLACE it into the world framebuffer.
        for &slot in &self.contrib {
            let s = slot as usize;
            let speed = zones.get(s).speed_multiplier;
            let zclock = scaled_clock(clock, speed);
            {
                let slot_mut = &mut self.slots[s];
                if let Some(effect) = slot_mut.effect.as_mut() {
                    effect.render(
                        gpu,
                        &zclock,
                        &view(&slot_mut.zone_tex),
                        slot_mut.mapping.0,
                        slot_mut.mapping.1,
                    );
                }
            }
            let slot_ref = &self.slots[s];
            self.placer.place(
                gpu,
                &slot_ref.zone_tex,
                &view(&slot_ref.placed_tex),
                slot_ref.footprint,
            );
        }

        // 3. This frame's per-cell overlap mask, into the persistent buffer.
        gpu.queue.write_buffer(
            &self.cell_mask,
            0,
            bytemuck::cast_slice(overlap.cell_mask_bytes()),
        );

        // 4. Fold the placed textures into `target` with the UNCHANGED blend
        //    machinery (`blend_zone`), ping-ponging over two persistent scratch
        //    textures. No per-frame Vec<ZoneComposite>, no GPU allocation. Zone
        //    dimmer/colour is applied here only on cells the snapshot marks
        //    overlapped (via cell_mask); elsewhere it is deferred to the output
        //    stage (§5.2.1) — this content does not touch that split, it just feeds
        //    the same cell_mask the M5 compositor expects.
        let Some((&last, rest)) = self.contrib.split_last() else {
            // Nothing contributes: the world framebuffer is black this frame.
            clear_black(gpu, target);
            return;
        };

        clear_black(gpu, &self.ping);
        let mut acc: &wgpu::Texture = &self.ping;
        let mut other: &wgpu::Texture = &self.pong;
        for &slot in rest {
            let s = slot as usize;
            let zs = &self.slots[s];
            self.compositor.blend_zone(
                gpu,
                acc,
                &zs.placed_tex,
                &view(other),
                zs.blend_mode,
                1.0,
                zone_scale(zones, s),
                &self.cell_mask,
                &zs.coverage,
            );
            std::mem::swap(&mut acc, &mut other);
        }
        // The last zone blends straight into the render loop's framebuffer.
        let s = last as usize;
        let zs = &self.slots[s];
        self.compositor.blend_zone(
            gpu,
            acc,
            &zs.placed_tex,
            &view(target),
            zs.blend_mode,
            1.0,
            zone_scale(zones, s),
            &self.cell_mask,
            &zs.coverage,
        );
    }
}

/// Clear an `Rgba16Float` texture to linear-light black — the fold's base and the
/// no-content frame. A bare render pass with a clear load op; no allocation.
fn clear_black(gpu: &Gpu, texture: &wgpu::Texture) {
    let v = view(texture);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("zone-clear"),
        });
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("zone-clear-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &v,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    gpu.queue.submit([encoder.finish()]);
}

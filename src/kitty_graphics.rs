use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as FmtWrite;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine;
use ratatui::layout::Rect;

use crate::app::state::AppState;
use crate::ghostty::{KittyImageDescriptor, KittyImageFormat, KittyImagePlacement};
use crate::layout::PaneId;
use crate::terminal::TerminalRuntimeRegistry;

pub(crate) mod output;
pub(crate) mod surface;

pub(crate) use output::{GraphicsOperation, GraphicsOutput};
use std::sync::Arc;

const KITTY_CHUNK_BYTES: usize = 3072;
pub(crate) const HEADLESS_GRAPHICS_TRANSACTION_BUDGET: usize =
    crate::protocol::MAX_GRAPHICS_FRAME_SIZE - crate::protocol::MAX_FRAME_SIZE;
const HOST_IMAGE_ID_BASE: u32 = 10_000;
/// Source pixels an upload keeps per pixel its placements are drawn at, unless the
/// client was started with `HOST_IMAGE_OVERSAMPLE_ENV`. Above one because a
/// terminal may report its cell size in CSS pixels (xterm.js does) and draw on a
/// screen of two or three device pixels to each.
const HOST_IMAGE_OVERSAMPLE: f64 = 2.0;
/// Sets that share for one client process, from 0.25 to 4. Whoever starts the
/// client knows the screen it draws to and the link it goes over, and herdr does
/// not: a page on a phone takes 1, a quarter of the pixels of the default.
const HOST_IMAGE_OVERSAMPLE_ENV: &str = "HERDR_KITTY_IMAGE_OVERSAMPLE";
/// An RGB or RGBA image whose placements need less than this share of its width
/// is uploaded scaled down, as PNG. A full-size screenshot shown as a thumbnail
/// was sent whole, about 9.6 MB of base64 for 1734x1040, on every upload.
const HOST_IMAGE_SCALE_BELOW: f64 = 0.75;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HostCellSize {
    pub width_px: u32,
    pub height_px: u32,
}

impl HostCellSize {
    pub(crate) fn is_known(self) -> bool {
        self.width_px > 0 && self.height_px > 0
    }
}

#[derive(Debug)]
struct HostPlacement {
    /// Client-local shared pixels; headless placements retain their inline data.
    raw_data: Option<Arc<[u8]>>,
    pane_id: PaneId,
    host_image_id: Option<u32>,
    area: Rect,
    cell_size: HostCellSize,
    source_key: HostSourceKey,
    placement: KittyImagePlacement,
    scrollback_offset: u32,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
enum HostSourceKey {
    Terminal {
        pane_id: PaneId,
        image_id: u32,
    },
    ClientSurface {
        scope: String,
        source: crate::protocol::SurfaceGraphicsSource,
    },
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct ImageSignature {
    image_width: u32,
    image_height: u32,
    format_code: u32,
    data_len: usize,
    data_fingerprint: u64,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct PlacementSignature {
    x: u16,
    y: u16,
    cols: u32,
    rows: u32,
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    x_offset: u32,
    y_offset: u32,
    z: i32,
    scrollback_offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClippedPlacement {
    x: u16,
    y: u16,
    cols: u32,
    rows: u32,
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    x_offset: u32,
    y_offset: u32,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct HostGraphicsCache {
    images: HashMap<u32, ImageSignature>,
    placements: HashMap<(u32, u32), PlacementSignature>,
    /// Host image currently backing each (pane, source image id) pair.
    sources: HashMap<HostSourceKey, u32>,
    replay_placements: bool,
    replayed_placements: HashSet<(u32, u32)>,
    /// Width and height of a host image uploaded scaled down; absent for one sent
    /// at its own size. Placements of a scaled image address its pixels in this size.
    pub(crate) uploaded_sizes: HashMap<u32, (u32, u32)>,
}

static KITTY_GRAPHICS_ENABLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_enabled(enabled: bool) {
    KITTY_GRAPHICS_ENABLED.store(enabled, Ordering::Release);
}

pub(crate) fn is_enabled() -> bool {
    KITTY_GRAPHICS_ENABLED.load(Ordering::Acquire)
}

pub(crate) fn image_transfer_estimated_size(data_len: usize) -> usize {
    let encoded = data_len.div_ceil(3).saturating_mul(4);
    let command_overhead = data_len.div_ceil(KITTY_CHUNK_BYTES).saturating_mul(16) + 1024;
    encoded.saturating_add(command_overhead)
}

fn encode_placement_update(
    cache: &mut HostGraphicsCache,
    placement: &HostPlacement,
    scaled_size: Option<(u32, u32)>,
) -> Option<GraphicsOutput> {
    let (clipped, format_code) = clipped_placement(placement)?;
    let host_id = placement_host_id(placement);
    let placement_id = host_placement_id(&placement.source_key, &placement.placement);
    let key = (host_id, placement_id);
    let image_signature = image_signature(placement, format_code);
    // An image sent at its own size serves any placement. One sent scaled serves
    // a placement that needs no more pixels than it has.
    let uploaded_size = cache.uploaded_sizes.get(&host_id).copied();
    let resolution_current = match (uploaded_size, scaled_size) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some((width, height)), Some((want_width, want_height))) => {
            width >= want_width && height >= want_height
        }
    };
    let image_current = cache.images.get(&host_id) == Some(&image_signature) && resolution_current;
    let source = placement
        .raw_data
        .as_deref()
        .unwrap_or(&placement.placement.data);
    // What an upload now sends: the scaled PNG where one is wanted and can be made,
    // else the image at its own size. Placements address the pixels actually sent.
    let scaled = if image_current || source.is_empty() {
        None
    } else {
        scaled_size.and_then(|(width, height)| {
            scaled_png(source, &placement.placement, format_code, width, height)
                .map(|png| (png, (width, height)))
        })
    };
    let sent_size = if image_current {
        uploaded_size
    } else {
        scaled.as_ref().map(|(_, size)| *size)
    };
    let clipped = scale_clipped(
        clipped,
        sent_size,
        placement.placement.image_width,
        placement.placement.image_height,
    );
    let placement_signature =
        placement_signature(clipped, placement.placement.z, placement.scrollback_offset);
    // An upload deletes the image's old placements, so it is always placed again.
    let placement_current = image_current
        && cache.placements.get(&key) == Some(&placement_signature)
        && (!cache.replay_placements || cache.replayed_placements.contains(&key));
    if image_current
        && placement_current
        && cache.sources.get(&placement.source_key) == Some(&host_id)
    {
        return None;
    }

    let mut bytes = Vec::new();
    let mut output = GraphicsOutput::default();
    if !image_current {
        // Bail out before touching the cache so a pending upload keeps the old image.
        if source.is_empty() {
            return None;
        }
        if cache.images.contains_key(&host_id) {
            encode_delete_image(&mut bytes, host_id);
            cache.placements.retain(|(id, _), _| *id != host_id);
            cache.replayed_placements.retain(|(id, _)| *id != host_id);
        }
        if let Some((png, size)) = scaled {
            output.push_bytes(std::mem::take(&mut bytes));
            output.operations.push(GraphicsOperation::Upload {
                control: format!("a=t,t=d,f=100,i={host_id},q=2"),
                data: Arc::from(png),
            });
            cache.uploaded_sizes.insert(host_id, size);
        } else {
            if let Some(data) = &placement.raw_data {
                output.push_bytes(std::mem::take(&mut bytes));
                output.operations.push(GraphicsOperation::Upload {
                    control: upload_control(placement, format_code, host_id),
                    data: Arc::clone(data),
                });
            } else {
                let control = upload_control(placement, format_code, host_id);
                encode_kitty_data(&mut bytes, &control, &placement.placement.data);
            }
            cache.uploaded_sizes.remove(&host_id);
        }
        cache.images.insert(host_id, image_signature);
    }

    release_superseded_source_image(&mut bytes, cache, placement.source_key.clone(), host_id);
    if !placement_current {
        encode_display_placement(
            &mut bytes,
            clipped,
            host_id,
            placement_id,
            placement.placement.z,
        );
    }
    cache.placements.insert(key, placement_signature);
    if cache.replay_placements {
        cache.replayed_placements.insert(key);
    }
    output.push_bytes(bytes);
    Some(output)
}

fn release_superseded_source_image(
    bytes: &mut Vec<u8>,
    cache: &mut HostGraphicsCache,
    source: HostSourceKey,
    host_id: u32,
) {
    let Some(previous) = cache.sources.insert(source, host_id) else {
        return;
    };
    if previous == host_id || cache.sources.values().any(|id| *id == previous) {
        return;
    }
    encode_delete_image(bytes, previous);
    cache.images.remove(&previous);
    cache.uploaded_sizes.remove(&previous);
    cache.placements.retain(|(id, _), _| *id != previous);
    cache.replayed_placements.retain(|(id, _)| *id != previous);
}

/// Encodes every placement change for one frame in a single linear pass.
fn encode_graphics_output(
    cache: &mut HostGraphicsCache,
    placements: &[HostPlacement],
) -> GraphicsOutput {
    let desired_sources = placements
        .iter()
        .map(|placement| placement.source_key.clone())
        .collect::<HashSet<_>>();
    let desired_placements = placements
        .iter()
        .filter_map(|placement| {
            clipped_placement(placement).map(|_| {
                let host_id = placement
                    .host_image_id
                    .unwrap_or_else(|| host_image_id(placement.pane_id, &placement.placement));
                (
                    host_id,
                    host_placement_id(&placement.source_key, &placement.placement),
                )
            })
        })
        .collect::<HashSet<_>>();
    cache
        .sources
        .retain(|source, _| desired_sources.contains(source));

    let mut output = GraphicsOutput::default();
    let mut stale = cache
        .placements
        .keys()
        .filter(|key| !desired_placements.contains(key))
        .copied()
        .collect::<Vec<_>>();
    stale.sort_unstable();
    for key @ (host_id, placement_id) in stale {
        let mut bytes = Vec::new();
        encode_delete_placement(&mut bytes, host_id, placement_id);
        output.push_bytes(bytes);
        cache.placements.remove(&key);
        cache.replayed_placements.remove(&key);
    }
    let scaled_sizes = scaled_upload_sizes(placements, client_image_oversample());
    for placement in placements {
        let scaled_size = scaled_sizes.get(&placement_host_id(placement)).copied();
        if let Some(transaction) = encode_placement_update(cache, placement, scaled_size) {
            output.extend(transaction);
        }
    }
    cache.replay_placements = false;
    cache.replayed_placements.clear();
    output
}

impl HostGraphicsCache {
    fn reset_replay(&mut self) {
        self.replay_placements = false;
        self.replayed_placements.clear();
    }

    pub(crate) fn request_placement_replay(&mut self) {
        if !self.replay_placements {
            self.replay_placements = true;
            self.replayed_placements.clear();
        }
    }

    pub(crate) fn clear_bytes(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for id in self.images.keys().copied().collect::<Vec<_>>() {
            encode_delete_image(&mut bytes, id);
        }
        self.images.clear();
        self.placements.clear();
        self.sources.clear();
        self.uploaded_sizes.clear();
        self.reset_replay();
        bytes
    }

    /// Forgets a host image the caller deleted from the terminal itself.
    pub(crate) fn forget_image(&mut self, id: u32) {
        self.images.remove(&id);
        self.uploaded_sizes.remove(&id);
        self.placements.retain(|(image, _), _| *image != id);
        self.sources.retain(|_, image| *image != id);
        self.replayed_placements.retain(|(image, _)| *image != id);
    }
}

fn collect_visible_placements(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: crate::ui::TabSurfaceView<'_>,
    cell_size: HostCellSize,
    delivered_images: &HashMap<HostSourceKey, ImageSignature>,
) -> Vec<HostPlacement> {
    let Some(target) = surface.target else {
        tracing::debug!("collect_visible_placements: no tab surface target");
        return Vec::new();
    };
    let ws_idx = target.workspace_index;
    if app
        .workspaces
        .get(ws_idx)
        .and_then(|workspace| workspace.tabs.get(target.tab_index))
        .is_none()
    {
        tracing::debug!(
            ws_idx,
            tab_idx = target.tab_index,
            "collect_visible_placements: no target tab"
        );
        return Vec::new();
    }

    tracing::debug!(
        ws_idx,
        terminal_runtimes_len = terminal_runtimes.len(),
        pane_infos_len = surface.pane_infos.len(),
        "collect_visible_placements: starting iteration"
    );
    let mut placements = Vec::new();
    for info in surface.pane_infos {
        let runtime = match app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id) {
            Some(rt) => rt,
            None => {
                tracing::debug!(pane_id = ?info.id, "collect_visible_placements: runtime not found");
                continue;
            }
        };
        let mut requested_images = HashSet::new();
        let pane_placements = runtime.kitty_image_placements_with_data_filter(|descriptor| {
            if descriptor.source_file {
                return false;
            }
            terminal_image_needs_data(info.id, descriptor, delivered_images, &mut requested_images)
        });
        if pane_placements.is_empty() {
            continue;
        }
        let scrollback_offset = runtime
            .scroll_metrics()
            .map(|m| m.offset_from_bottom as u32)
            .unwrap_or(0);
        for placement in pane_placements {
            placements.push(HostPlacement {
                raw_data: None,
                pane_id: info.id,
                host_image_id: None,
                area: info.inner_rect,
                cell_size,
                source_key: HostSourceKey::Terminal {
                    pane_id: info.id,
                    image_id: placement.image_id,
                },
                placement,
                scrollback_offset,
            });
        }
    }
    tracing::debug!(
        placements_len = placements.len(),
        "collect_visible_placements: done"
    );
    placements
}

fn terminal_image_needs_data(
    pane_id: PaneId,
    descriptor: KittyImageDescriptor,
    delivered_images: &HashMap<HostSourceKey, ImageSignature>,
    requested_images: &mut HashSet<(HostSourceKey, ImageSignature)>,
) -> bool {
    let format_code = kitty_format_code(descriptor.format);
    let signature = image_signature_from_descriptor(descriptor, format_code);
    let source = HostSourceKey::Terminal {
        pane_id,
        image_id: descriptor.image_id,
    };
    delivered_images.get(&source).copied() != Some(signature)
        && requested_images.insert((source, signature))
}

fn host_image_id(pane_id: PaneId, placement: &KittyImagePlacement) -> u32 {
    let format_code = kitty_format_code(placement.format);
    host_image_id_for_signature(
        pane_id,
        ImageSignature {
            image_width: placement.image_width,
            image_height: placement.image_height,
            format_code,
            data_len: placement.data_len,
            data_fingerprint: placement.data_fingerprint,
        },
    )
}

fn host_image_id_for_signature(pane_id: PaneId, signature: ImageSignature) -> u32 {
    let mut hasher = DefaultHasher::new();
    pane_id.raw().hash(&mut hasher);
    signature.hash(&mut hasher);
    HOST_IMAGE_ID_BASE + ((hasher.finish() as u32) % 900_000)
}

fn host_placement_id(source_key: &HostSourceKey, placement: &KittyImagePlacement) -> u32 {
    let mut hasher = DefaultHasher::new();
    match source_key {
        HostSourceKey::Terminal { pane_id, .. } => pane_id.raw().hash(&mut hasher),
        HostSourceKey::ClientSurface { scope, source } => {
            "client.surface".hash(&mut hasher);
            scope.hash(&mut hasher);
            source.hash(&mut hasher);
        }
    }
    placement.image_id.hash(&mut hasher);
    placement.placement_id.hash(&mut hasher);
    1 + ((hasher.finish() as u32) % 900_000)
}

#[cfg(unix)]
pub(crate) fn encode_kitty_regular_file(
    out: &mut Vec<u8>,
    leading: &[u8],
    control: &str,
    path: &str,
) {
    let payload = base64::engine::general_purpose::STANDARD.encode(path.as_bytes());
    out.extend_from_slice(b"\x1b7");
    out.extend_from_slice(leading);
    let _ = write!(out, "\x1b_G{control},t=f;{payload}\x1b\\");
    out.extend_from_slice(b"\x1b8");
}

pub(crate) fn encode_delete_image(out: &mut Vec<u8>, id: u32) {
    let _ = write!(out, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\");
}

fn encode_delete_placement(out: &mut Vec<u8>, host_id: u32, host_placement_id: u32) {
    let _ = write!(
        out,
        "\x1b_Ga=d,d=i,i={host_id},p={host_placement_id},q=2;\x1b\\"
    );
}

fn upload_control(placement: &HostPlacement, format_code: u32, host_id: u32) -> String {
    format!(
        "a=t,t=d,f={format_code},s={},v={},i={host_id},q=2",
        placement.placement.image_width, placement.placement.image_height,
    )
}

fn encode_display_placement(
    out: &mut Vec<u8>,
    clipped: ClippedPlacement,
    host_id: u32,
    host_placement_id: u32,
    z: i32,
) {
    let _ = write!(out, "\x1b[{};{}H", clipped.y + 1, clipped.x + 1);
    let mut control = format!(
        "a=p,i={host_id},p={host_placement_id},c={},r={},z={z},C=1,q=2",
        clipped.cols, clipped.rows,
    );
    append_placement_controls(&mut control, clipped);
    let _ = write!(out, "\x1b_G{control};\x1b\\");
}

fn append_placement_controls(control: &mut String, clipped: ClippedPlacement) {
    if clipped.source_x > 0 {
        let _ = write!(control, ",x={}", clipped.source_x);
    }
    if clipped.source_y > 0 {
        let _ = write!(control, ",y={}", clipped.source_y);
    }
    if clipped.source_width > 0 {
        let _ = write!(control, ",w={}", clipped.source_width);
    }
    if clipped.source_height > 0 {
        let _ = write!(control, ",h={}", clipped.source_height);
    }
    if clipped.x_offset > 0 {
        let _ = write!(control, ",X={}", clipped.x_offset);
    }
    if clipped.y_offset > 0 {
        let _ = write!(control, ",Y={}", clipped.y_offset);
    }
}

fn clipped_placement(placement: &HostPlacement) -> Option<(ClippedPlacement, u32)> {
    if placement.area.width == 0 || placement.area.height == 0 {
        tracing::debug!(
            area_w = placement.area.width,
            area_h = placement.area.height,
            "clipped_placement: area zero"
        );
        return None;
    }
    let render = placement.placement.render;
    if render.grid_cols == 0 || render.grid_rows == 0 {
        tracing::debug!(
            grid_cols = render.grid_cols,
            grid_rows = render.grid_rows,
            "clipped_placement: grid zero"
        );
        return None;
    }
    let format_code = kitty_format_code(placement.placement.format);

    let left_clip_cells = if render.viewport_col < 0 {
        render.viewport_col.saturating_neg() as u32
    } else {
        0
    };
    let top_clip_cells = if render.viewport_row < 0 {
        render.viewport_row.saturating_neg() as u32
    } else {
        0
    };
    let viewport_col = render.viewport_col.max(0) as u32;
    let viewport_row = render.viewport_row.max(0) as u32;
    tracing::debug!(
        viewport_col = viewport_col,
        viewport_row = viewport_row,
        area_w = placement.area.width,
        area_h = placement.area.height,
        scrollback_offset = placement.scrollback_offset,
        raw_viewport_row = render.viewport_row,
        cond1 = viewport_col >= placement.area.width as u32,
        cond2 = viewport_row >= placement.area.height as u32,
        "clipped_placement: viewport check"
    );
    if viewport_col >= placement.area.width as u32 || viewport_row >= placement.area.height as u32 {
        return None;
    }

    let visible_cols = render
        .grid_cols
        .saturating_sub(left_clip_cells)
        .min(placement.area.width as u32 - viewport_col);
    let visible_rows = render
        .grid_rows
        .saturating_sub(top_clip_cells)
        .min(placement.area.height as u32 - viewport_row);
    tracing::debug!(
        visible_cols = visible_cols,
        visible_rows = visible_rows,
        left_clip_cells = left_clip_cells,
        top_clip_cells = top_clip_cells,
        "clipped_placement: visible dims check"
    );
    if visible_cols == 0 || visible_rows == 0 {
        return None;
    }

    let source_width = if render.source_width == 0 {
        placement.placement.image_width
    } else {
        render.source_width
    };
    let source_height = if render.source_height == 0 {
        placement.placement.image_height
    } else {
        render.source_height
    };
    let pixel_width = render
        .pixel_width
        .max(
            render
                .grid_cols
                .saturating_mul(placement.cell_size.width_px),
        )
        .max(1);
    let pixel_height = render
        .pixel_height
        .max(
            render
                .grid_rows
                .saturating_mul(placement.cell_size.height_px),
        )
        .max(1);

    let crop_left_px = left_clip_cells.saturating_mul(placement.cell_size.width_px);
    let crop_top_px = top_clip_cells.saturating_mul(placement.cell_size.height_px);
    let visible_width_px = visible_cols.saturating_mul(placement.cell_size.width_px);
    let visible_height_px = visible_rows.saturating_mul(placement.cell_size.height_px);

    let source_x = render.source_x + scale_pixels(crop_left_px, source_width, pixel_width);
    let source_y = render.source_y + scale_pixels(crop_top_px, source_height, pixel_height);
    let source_width = scale_pixels(visible_width_px, source_width, pixel_width)
        .max(1)
        .min(placement.placement.image_width.saturating_sub(source_x));
    let source_height = scale_pixels(visible_height_px, source_height, pixel_height)
        .max(1)
        .min(placement.placement.image_height.saturating_sub(source_y));

    if source_width == 0 || source_height == 0 {
        tracing::debug!(
            source_width = source_width,
            source_height = source_height,
            image_width = placement.placement.image_width,
            image_height = placement.placement.image_height,
            "clipped_placement: source dims zero"
        );
        return None;
    }

    tracing::debug!("clipped_placement: success");
    Some((
        ClippedPlacement {
            x: placement.area.x + viewport_col as u16,
            y: placement.area.y + viewport_row as u16,
            cols: visible_cols,
            rows: visible_rows,
            source_x,
            source_y,
            source_width,
            source_height,
            x_offset: if left_clip_cells == 0 {
                placement.placement.x_offset
            } else {
                0
            },
            y_offset: if top_clip_cells == 0 {
                placement.placement.y_offset
            } else {
                0
            },
        },
        format_code,
    ))
}

fn scale_pixels(value: u32, source: u32, dest: u32) -> u32 {
    ((value as u64).saturating_mul(source as u64) / dest.max(1) as u64).min(u32::MAX as u64) as u32
}

/// This client's oversample, read once from its environment.
fn client_image_oversample() -> f64 {
    static VALUE: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        let raw = std::env::var(HOST_IMAGE_OVERSAMPLE_ENV).ok();
        let value = parse_oversample(raw.as_deref());
        if raw.is_some() && value.is_none() {
            tracing::warn!(
                value = raw.as_deref(),
                "{HOST_IMAGE_OVERSAMPLE_ENV} is not a number from 0.25 to 4; using {HOST_IMAGE_OVERSAMPLE}"
            );
        }
        value.unwrap_or(HOST_IMAGE_OVERSAMPLE)
    })
}

fn parse_oversample(raw: Option<&str>) -> Option<f64> {
    raw.and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|value| (0.25..=4.0).contains(value))
}

fn scale_pixels_up(value: u32, source: u32, dest: u32) -> u32 {
    (value as u64)
        .saturating_mul(source as u64)
        .div_ceil(dest.max(1) as u64)
        .min(u32::MAX as u64) as u32
}

fn placement_host_id(placement: &HostPlacement) -> u32 {
    placement
        .host_image_id
        .unwrap_or_else(|| host_image_id(placement.pane_id, &placement.placement))
}

/// The size to upload each RGB or RGBA host image at this frame, for an image
/// its visible placements draw well under its own size. The largest share any
/// placement draws at decides it, so no placement is short of pixels.
fn scaled_upload_sizes(placements: &[HostPlacement], oversample: f64) -> HashMap<u32, (u32, u32)> {
    let mut shares: HashMap<u32, (f64, u32, u32)> = HashMap::new();
    for placement in placements {
        let image = &placement.placement;
        if !matches!(image.format, KittyImageFormat::Rgb | KittyImageFormat::Rgba)
            || image.image_width == 0
            || image.image_height == 0
            || !placement.cell_size.is_known()
            || clipped_placement(placement).is_none()
        {
            continue;
        }
        let render = image.render;
        let source_width = if render.source_width == 0 {
            image.image_width
        } else {
            render.source_width
        };
        let source_height = if render.source_height == 0 {
            image.image_height
        } else {
            render.source_height
        };
        let drawn_width = render.pixel_width.max(
            render
                .grid_cols
                .saturating_mul(placement.cell_size.width_px),
        );
        let drawn_height = render.pixel_height.max(
            render
                .grid_rows
                .saturating_mul(placement.cell_size.height_px),
        );
        let share = (drawn_width as f64 / source_width.max(1) as f64)
            .max(drawn_height as f64 / source_height.max(1) as f64)
            * oversample;
        let entry = shares.entry(placement_host_id(placement)).or_insert((
            0.0,
            image.image_width,
            image.image_height,
        ));
        entry.0 = entry.0.max(share);
    }
    shares
        .into_iter()
        .filter(|(_, (share, _, _))| *share > 0.0 && *share < HOST_IMAGE_SCALE_BELOW)
        .map(|(id, (share, width, height))| {
            let scaled = |side: u32| ((side as f64 * share).ceil() as u32).clamp(1, side);
            (id, (scaled(width), scaled(height)))
        })
        .collect()
}

/// A placement's source rectangle moved into the pixels of an image sent at `sent`.
fn scale_clipped(
    mut clipped: ClippedPlacement,
    sent: Option<(u32, u32)>,
    width: u32,
    height: u32,
) -> ClippedPlacement {
    let Some((sent_width, sent_height)) = sent else {
        return clipped;
    };
    let x = scale_pixels(clipped.source_x, sent_width, width).min(sent_width - 1);
    let y = scale_pixels(clipped.source_y, sent_height, height).min(sent_height - 1);
    let end_x =
        scale_pixels_up(clipped.source_x + clipped.source_width, sent_width, width).min(sent_width);
    let end_y = scale_pixels_up(
        clipped.source_y + clipped.source_height,
        sent_height,
        height,
    )
    .min(sent_height);
    clipped.source_x = x;
    clipped.source_y = y;
    clipped.source_width = end_x.saturating_sub(x).max(1);
    clipped.source_height = end_y.saturating_sub(y).max(1);
    clipped
}

/// The image box-filtered down to `width` x `height` and encoded as PNG, or nothing
/// for a format or a buffer this cannot read.
fn scaled_png(
    data: &[u8],
    image: &KittyImagePlacement,
    format_code: u32,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let (channels, color) = match format_code {
        24 => (3, png::ColorType::Rgb),
        32 => (4, png::ColorType::Rgba),
        _ => return None,
    };
    let (source_width, source_height) = (image.image_width, image.image_height);
    if data.len() != source_width as usize * source_height as usize * channels {
        return None;
    }
    let pixels = downscale_box(data, source_width, source_height, channels, width, height);
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        // Best costs little on an image already cut to its drawn size: 47 KB
        // became 45 KB at 1x, and 85 KB 74 KB at 2x, for a 1734x1040 screenshot.
        encoder.set_compression(png::Compression::Best);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&pixels).ok()?;
        writer.finish().ok()?;
    }
    Some(png)
}

/// Each output pixel is the mean of the source pixels under it.
fn downscale_box(
    data: &[u8],
    source_width: u32,
    source_height: u32,
    channels: usize,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let span = |out: u32, out_len: u32, source_len: u32| {
        let start = scale_pixels(out, source_len, out_len);
        let end = scale_pixels_up(out + 1, source_len, out_len).clamp(start + 1, source_len);
        start as usize..end as usize
    };
    let mut pixels = vec![0_u8; width as usize * height as usize * channels];
    let mut sum = [0_u64; 4];
    for out_y in 0..height {
        let rows = span(out_y, height, source_height);
        for out_x in 0..width {
            let cols = span(out_x, width, source_width);
            sum[..channels].fill(0);
            for row in rows.clone() {
                let line = row * source_width as usize;
                for col in cols.clone() {
                    let at = (line + col) * channels;
                    for (total, value) in sum[..channels].iter_mut().zip(&data[at..at + channels]) {
                        *total += u64::from(*value);
                    }
                }
            }
            let count = (rows.len() * cols.len()) as u64;
            let at = (out_y as usize * width as usize + out_x as usize) * channels;
            for (out, total) in pixels[at..at + channels].iter_mut().zip(&sum[..channels]) {
                *out = ((total + count / 2) / count) as u8;
            }
        }
    }
    pixels
}

fn image_signature(placement: &HostPlacement, format_code: u32) -> ImageSignature {
    ImageSignature {
        image_width: placement.placement.image_width,
        image_height: placement.placement.image_height,
        format_code,
        data_len: placement.placement.data_len,
        data_fingerprint: placement.placement.data_fingerprint,
    }
}

fn image_signature_from_descriptor(
    descriptor: KittyImageDescriptor,
    format_code: u32,
) -> ImageSignature {
    ImageSignature {
        image_width: descriptor.image_width,
        image_height: descriptor.image_height,
        format_code,
        data_len: descriptor.data_len,
        data_fingerprint: descriptor.data_fingerprint,
    }
}

fn placement_signature(
    clipped: ClippedPlacement,
    z: i32,
    scrollback_offset: u32,
) -> PlacementSignature {
    PlacementSignature {
        x: clipped.x,
        y: clipped.y,
        cols: clipped.cols,
        rows: clipped.rows,
        source_x: clipped.source_x,
        source_y: clipped.source_y,
        source_width: clipped.source_width,
        source_height: clipped.source_height,
        x_offset: clipped.x_offset,
        y_offset: clipped.y_offset,
        z,
        scrollback_offset,
    }
}

fn kitty_format_code(format: KittyImageFormat) -> u32 {
    match format {
        KittyImageFormat::Rgb => 24,
        KittyImageFormat::Rgba => 32,
        KittyImageFormat::Png => 100,
    }
}

fn encode_kitty_data(out: &mut Vec<u8>, control: &str, data: &[u8]) {
    write_kitty_data(out, control, data).expect("writing to Vec cannot fail");
}

pub(crate) fn write_kitty_data(
    out: &mut impl Write,
    control: &str,
    data: &[u8],
) -> std::io::Result<()> {
    let mut chunks = data.chunks(KITTY_CHUNK_BYTES).peekable();
    let Some(first) = chunks.next() else {
        return Ok(());
    };
    let more = if chunks.peek().is_some() { 1 } else { 0 };
    let encoded = base64::engine::general_purpose::STANDARD.encode(first);
    write!(out, "\x1b_G{control},m={more};{encoded}\x1b\\")?;

    while let Some(chunk) = chunks.next() {
        let more = if chunks.peek().is_some() { 1 } else { 0 };
        let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
        write!(out, "\x1b_Gm={more};{encoded}\x1b\\")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_updates_match_inline_for_rgb_and_rgba() {
        for format in [KittyImageFormat::Rgb, KittyImageFormat::Rgba] {
            let mut first = test_placement(0, 0);
            first.placement.format = format;
            first.placement.data = vec![37; KITTY_CHUNK_BYTES * 2 + 7];
            first.placement.data_len = first.placement.data.len();
            let data: Arc<[u8]> = Arc::from(first.placement.data.clone());
            let mut second = test_placement(3, 0);
            second.placement = first.placement.clone();
            second.placement.placement_id += 1;
            let mut placements = [first, second];
            let mut inline_cache = HostGraphicsCache::default();
            let expected =
                encode_graphics_output(&mut inline_cache, &placements).into_inline_bytes();
            for placement in &mut placements {
                placement.placement.data.clear();
                placement.raw_data = Some(Arc::clone(&data));
            }
            let mut cache = HostGraphicsCache::default();
            let output = encode_graphics_output(&mut cache, &placements);
            assert_eq!(output.clone().into_inline_bytes(), expected);
            let uploads = output
                .operations
                .iter()
                .filter_map(|op| match op {
                    GraphicsOperation::Upload { data, .. } => Some(data),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(uploads.len(), 1);
            assert!(Arc::ptr_eq(uploads[0], &data));
            assert_eq!(cache.placements.len(), 2);
        }
    }

    fn test_placement(viewport_col: i32, viewport_row: i32) -> HostPlacement {
        HostPlacement {
            raw_data: None,
            pane_id: PaneId::from_raw(1),
            host_image_id: None,
            area: Rect::new(0, 0, 20, 10),
            cell_size: HostCellSize {
                width_px: 10,
                height_px: 10,
            },
            source_key: HostSourceKey::Terminal {
                pane_id: PaneId::from_raw(1),
                image_id: 7,
            },
            scrollback_offset: 0,
            placement: KittyImagePlacement {
                image_id: 7,
                placement_id: 3,
                z: 0,
                x_offset: 0,
                y_offset: 0,
                image_width: 30,
                image_height: 30,
                format: KittyImageFormat::Rgba,
                data_len: 30 * 30 * 4,
                data_fingerprint: 42,
                data: vec![255; 30 * 30 * 4],
                source_file: None,
                render: crate::ghostty::KittyPlacementRenderInfo {
                    pixel_width: 0,
                    pixel_height: 0,
                    grid_cols: 3,
                    grid_rows: 3,
                    viewport_col,
                    viewport_row,
                    source_x: 0,
                    source_y: 0,
                    source_width: 0,
                    source_height: 0,
                },
            },
        }
    }

    fn update(
        cache: &mut HostGraphicsCache,
        placements: &[HostPlacement],
        replay: bool,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        if replay {
            cache.request_placement_replay();
        }
        bytes.extend(encode_graphics_output(cache, placements).into_inline_bytes());
        bytes
    }

    /// A 400x400 RGBA image drawn in `cols` x `rows` cells of 10 px.
    fn large_placement(cols: u32, rows: u32, viewport: (i32, i32)) -> HostPlacement {
        let mut placement = test_placement(viewport.0, viewport.1);
        placement.placement.image_width = 400;
        placement.placement.image_height = 400;
        placement.placement.data = (0..400 * 400 * 4).map(|i| (i % 251) as u8).collect();
        placement.placement.data_len = placement.placement.data.len();
        placement.placement.render.grid_cols = cols;
        placement.placement.render.grid_rows = rows;
        placement
    }

    fn uploads(output: &GraphicsOutput) -> Vec<(&str, &[u8])> {
        output
            .operations
            .iter()
            .filter_map(|op| match op {
                GraphicsOperation::Upload { control, data } => Some((control.as_str(), &data[..])),
                GraphicsOperation::Bytes(bytes) => String::from_utf8_lossy(bytes)
                    .contains("a=t")
                    .then_some(("inline", &[][..])),
            })
            .collect()
    }

    fn png_size(data: &[u8]) -> (u32, u32) {
        let reader = png::Decoder::new(std::io::Cursor::new(data))
            .read_info()
            .expect("a PNG");
        (reader.info().width, reader.info().height)
    }

    #[test]
    fn image_drawn_well_under_its_size_is_uploaded_scaled_as_png() {
        let mut placement = large_placement(3, 3, (0, 0));
        placement.raw_data = Some(Arc::from(std::mem::take(&mut placement.placement.data)));
        let mut cache = HostGraphicsCache::default();
        let output = encode_graphics_output(&mut cache, &[placement]);
        let uploads = uploads(&output);
        assert_eq!(uploads.len(), 1);
        let (control, data) = uploads[0];
        assert!(control.starts_with("a=t,t=d,f=100,"), "{control}");
        // 30 px drawn, twice that kept: 60 of 400.
        assert_eq!(png_size(data), (60, 60));
        let text = String::from_utf8_lossy(&output.into_inline_bytes()).into_owned();
        assert!(
            text.contains("a=p,") && text.contains(",w=60,h=60"),
            "{text}"
        );
    }

    #[test]
    fn scaled_image_is_sent_again_whole_when_drawn_near_its_size() {
        let mut cache = HostGraphicsCache::default();
        let small = update(&mut cache, &[large_placement(3, 3, (0, 0))], false);
        assert!(String::from_utf8_lossy(&small).contains("f=100"));
        let moved = update(&mut cache, &[large_placement(3, 3, (1, 0))], false);
        assert!(!String::from_utf8_lossy(&moved).contains("a=t"));
        let large = update(&mut cache, &[large_placement(20, 10, (0, 0))], false);
        let text = String::from_utf8_lossy(&large);
        assert!(text.contains("a=t,t=d,f=32,s=400,v=400"), "{text}");
        assert!(cache.uploaded_sizes.is_empty());
    }

    #[test]
    fn cropped_placement_of_a_scaled_image_addresses_its_pixels() {
        let mut cache = HostGraphicsCache::default();
        let bytes = update(&mut cache, &[large_placement(3, 3, (-1, -1))], false);
        let text = String::from_utf8_lossy(&bytes);
        // One cell of three cropped from the top and the left: source 133..399 of
        // 400, which is 19..60 of the 60 sent.
        assert!(text.contains(",x=19,y=19,w=41,h=41"), "{text}");
    }

    #[test]
    fn oversample_is_the_clients_own_and_keeps_to_its_range() {
        assert_eq!(parse_oversample(Some("1")), Some(1.0));
        assert_eq!(parse_oversample(Some(" 1.5 ")), Some(1.5));
        for bad in ["0", "0.1", "5", "NaN", "inf", "two", ""] {
            assert_eq!(parse_oversample(Some(bad)), None, "{bad}");
        }
        assert_eq!(parse_oversample(None), None);

        let placement = large_placement(3, 3, (0, 0));
        let id = placement_host_id(&placement);
        let sizes = |oversample| scaled_upload_sizes(std::slice::from_ref(&placement), oversample);
        assert_eq!(sizes(1.0).get(&id), Some(&(30, 30)));
        assert_eq!(sizes(HOST_IMAGE_OVERSAMPLE).get(&id), Some(&(60, 60)));
    }

    #[test]
    fn box_filter_averages_the_pixels_under_each_output_pixel() {
        let rgb = [0, 0, 0, 10, 20, 30, 20, 40, 60, 30, 60, 90];
        assert_eq!(downscale_box(&rgb, 2, 2, 3, 1, 1), vec![15, 30, 45]);
        assert_eq!(downscale_box(&rgb, 2, 2, 3, 2, 2), rgb.to_vec());
    }

    #[test]
    fn terminal_placement_id_preserves_legacy_identity() {
        let placement = test_placement(0, 0);
        let mut legacy = DefaultHasher::new();
        placement.pane_id.raw().hash(&mut legacy);
        placement.placement.image_id.hash(&mut legacy);
        placement.placement.placement_id.hash(&mut legacy);
        let expected = 1 + ((legacy.finish() as u32) % 900_000);

        assert_eq!(
            host_placement_id(&placement.source_key, &placement.placement),
            expected
        );
    }

    #[cfg(unix)]
    #[test]
    fn regular_file_command_is_rgba_quiet_zero_and_path_encoded() {
        let mut bytes = Vec::new();
        encode_kitty_regular_file(
            &mut bytes,
            b"\x1b[2;3H",
            "a=T,f=32,s=3,v=2,i=42,p=7,c=3,r=2,z=0,C=1,q=0",
            "/private/frame",
        );
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("\x1b7\x1b[2;3H\x1b_Ga=T,f=32"));
        assert!(text.contains(",C=1,q=0,t=f;L3ByaXZhdGUvZnJhbWU="));
        assert!(text.ends_with("\x1b\\\x1b8"));
    }

    #[test]
    fn clipped_placement_handles_positive_viewport_without_wrapping() {
        let placement = test_placement(2, 2);
        let (clipped, _) = clipped_placement(&placement).expect("visible placement");

        assert_eq!(clipped.x, 2);
        assert_eq!(clipped.y, 2);
        assert_eq!(clipped.cols, 3);
        assert_eq!(clipped.rows, 3);
        assert_eq!(clipped.source_x, 0);
        assert_eq!(clipped.source_y, 0);
    }

    #[test]
    fn clipped_placement_crops_negative_viewport_offsets() {
        let placement = test_placement(-1, -1);
        let (clipped, _) = clipped_placement(&placement).expect("partially visible placement");

        assert_eq!(clipped.x, 0);
        assert_eq!(clipped.y, 0);
        assert_eq!(clipped.cols, 2);
        assert_eq!(clipped.rows, 2);
        assert_eq!(clipped.source_x, 10);
        assert_eq!(clipped.source_y, 10);
    }

    #[test]
    fn graphics_update_uploads_once_then_repositions_only() {
        let mut cache = HostGraphicsCache::default();
        let first = update(&mut cache, &[test_placement(0, 0)], false);
        assert!(String::from_utf8_lossy(&first).contains("a=t"));
        assert!(String::from_utf8_lossy(&first).contains("a=p"));
        assert!(update(&mut cache, &[test_placement(0, 0)], false).is_empty());

        let mut changed = test_placement(0, 0);
        changed.placement.z = 1;
        for placement in [changed, test_placement(0, 1)] {
            let bytes = update(&mut cache, &[placement], false);
            assert!(!String::from_utf8_lossy(&bytes).contains("a=t"));
            assert!(String::from_utf8_lossy(&bytes).contains("a=p"));
        }
    }

    #[test]
    fn changed_image_without_data_keeps_the_previous_image() {
        // Client surfaces keep one host image id per source across revisions.
        let placement = || {
            let mut placement = test_placement(0, 0);
            placement.host_image_id = Some(77);
            placement
        };
        let mut cache = HostGraphicsCache::default();
        let _ = encode_graphics_output(&mut cache, &[placement()]);
        let mut pending = placement();
        pending.placement.data_fingerprint = 43;
        pending.placement.data.clear();
        let images = cache.images.clone();
        let placements = cache.placements.clone();
        let output = encode_graphics_output(&mut cache, &[pending]);
        assert!(output.into_inline_bytes().is_empty());
        assert_eq!(cache.images, images);
        assert_eq!(cache.placements, placements);
    }

    #[test]
    fn view_change_redisplays_unchanged_visible_placement() {
        let mut cache = HostGraphicsCache::default();
        update(&mut cache, &[test_placement(0, 0)], false);
        assert_eq!(cache.placements.len(), 1);
        let bytes = update(&mut cache, &[test_placement(0, 0)], true);
        assert!(!String::from_utf8_lossy(&bytes).contains("a=t"));
        assert!(String::from_utf8_lossy(&bytes).contains("a=p"));
        assert_eq!(cache.placements.len(), 1);
    }

    #[test]
    fn surface_reset_deletes_then_reuploads_and_redisplays_placement() {
        let mut cache = HostGraphicsCache::default();
        update(&mut cache, &[test_placement(0, 0)], false);
        assert_eq!((cache.images.len(), cache.placements.len()), (1, 1));
        let mut bytes = cache.clear_bytes();
        bytes.extend(update(&mut cache, &[test_placement(0, 0)], false));
        let redisplay = String::from_utf8_lossy(&bytes);
        assert!(redisplay.contains("a=d,d=I"));
        assert!(redisplay.contains("a=t"));
        assert!(redisplay.contains("a=p"));
        assert_eq!((cache.images.len(), cache.placements.len()), (1, 1));
    }

    #[test]
    fn scrollback_offset_change_redisplays_placement() {
        let mut cache = HostGraphicsCache::default();
        update(&mut cache, &[test_placement(0, 0)], false);
        let mut scrolled = test_placement(0, 0);
        scrolled.scrollback_offset = 3;
        let bytes = update(&mut cache, &[scrolled], false);
        assert!(!String::from_utf8_lossy(&bytes).contains("a=t"));
        assert!(String::from_utf8_lossy(&bytes).contains("a=p"));
    }

    #[test]
    fn changing_first_source_does_not_starve_second_source() {
        let terminal = |id| {
            let mut placement = test_placement(0, 0);
            placement.placement.image_id = id;
            placement.placement.data_fingerprint = u64::from(id);
            placement.source_key = HostSourceKey::Terminal {
                pane_id: placement.pane_id,
                image_id: id,
            };
            placement
        };
        let second = terminal(99).source_key;
        let mut cache = HostGraphicsCache::default();
        for id in 1..=3 {
            let _ = encode_graphics_output(&mut cache, &[terminal(id), terminal(99)]);
            assert!(cache.sources.contains_key(&second));
        }
    }

    #[test]
    fn terminal_image_data_requests_deduplicate_and_reconsider_changed_signatures() {
        let pane_id = PaneId::from_raw(1);
        let descriptor = KittyImageDescriptor {
            image_id: 7,
            placement_id: 1,
            image_width: 3456,
            image_height: 2234,
            format: KittyImageFormat::Rgba,
            data_len: 3456 * 2234 * 4,
            data_fingerprint: 42,
            source_file: false,
        };
        let mut requested = HashSet::new();
        assert!(terminal_image_needs_data(
            pane_id,
            descriptor,
            &HashMap::new(),
            &mut requested,
        ));
        let mut second_placement = descriptor;
        second_placement.placement_id = 2;
        assert!(!terminal_image_needs_data(
            pane_id,
            second_placement,
            &HashMap::new(),
            &mut requested,
        ));

        let signature = image_signature_from_descriptor(descriptor, 32);
        let source = HostSourceKey::Terminal {
            pane_id,
            image_id: descriptor.image_id,
        };
        let delivered = HashMap::from([(source, signature)]);
        let mut requested = HashSet::new();
        assert!(!terminal_image_needs_data(
            pane_id,
            descriptor,
            &delivered,
            &mut requested,
        ));
        let mut changed = descriptor;
        changed.data_fingerprint += 1;
        assert!(terminal_image_needs_data(
            pane_id,
            changed,
            &delivered,
            &mut requested,
        ));
    }
}

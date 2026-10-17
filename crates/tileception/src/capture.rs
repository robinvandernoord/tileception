//! Screen capture: wlr-screencopy for grim, slurp and screen sharing through
//! xdg-desktop-portal-wlr, and `tileception msg screenshot <file.png>`.
//!
//! Captures are queued and taken on the next frame. Shared memory buffers and
//! PNG files are filled by rendering the scene into an off-screen texture and
//! reading it back; GPU buffers (dmabufs, which screen sharing uses) are
//! rendered into directly.

use std::{path::PathBuf, time::Duration};

use smithay::{
    backend::{
        allocator::{Buffer as _, Fourcc, dmabuf::Dmabuf},
        renderer::{
            Bind, ExportMem, Offscreen, TextureMapping, damage::OutputDamageTracker, gles::GlesRenderer,
            gles::GlesTexture,
        },
    },
    reexports::{
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            protocol::{wl_buffer::WlBuffer, wl_shm},
        },
    },
    utils::{Buffer, Clock, Monotonic, Rectangle, Size, Transform},
    wayland::{
        dmabuf::get_dmabuf,
        shm::{with_buffer_contents, with_buffer_contents_mut},
    },
};

use crate::{Tileception, render::OutputElement};

/// Where a finished capture goes.
pub enum CaptureTarget {
    File(PathBuf),
    Screencopy {
        frame: ZwlrScreencopyFrameV1,
        buffer: WlBuffer,
        damage: bool,
    },
    /// A screencopy into a GPU buffer of the whole output.
    Dmabuf {
        frame: ZwlrScreencopyFrameV1,
        dmabuf: Dmabuf,
        damage: bool,
    },
}

/// What is on screen, ready to be rendered into each capture's buffer.
struct Scene {
    elements: Vec<OutputElement>,
    size: (i32, i32),
    clear: [f32; 4],
}

pub struct PendingCapture {
    /// Part of the output, in pixels.
    pub region: Rectangle<i32, Buffer>,
    pub target: CaptureTarget,
}

/// What a screencopy frame was asked to capture.
pub struct FrameData {
    region: Rectangle<i32, Buffer>,
}

pub fn init_screencopy(dh: &DisplayHandle) {
    dh.create_global::<Tileception, ZwlrScreencopyManagerV1, _>(3, ());
}

impl Tileception {
    /// The whole output, in pixels.
    fn output_region(&self) -> Rectangle<i32, Buffer> {
        Rectangle::from_size(self.output_pixels().into())
    }

    /// Queues a screenshot of the whole output into a PNG file.
    pub fn screenshot(&mut self, path: PathBuf) {
        let region = self.output_region();
        self.pending_captures.push(PendingCapture { region, target: CaptureTarget::File(path) });
    }

    /// Takes the queued captures. Called by the backend once per frame, with
    /// its renderer.
    pub fn process_captures(&mut self, renderer: &mut GlesRenderer) {
        if self.pending_captures.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_captures);
        if matches!(self.lock, crate::lock::LockState::BuiltIn) {
            self.wallpaper_buffer();
        }
        let wallpaper = if self.is_locked() { None } else { self.wallpaper_buffer() };
        let mut elements = self.render_elements(renderer, self.now());
        if let Some(buffer) = wallpaper {
            crate::render::push_image(&mut elements, renderer, &buffer, (0.0, 0.0), self.scale);
        }
        let scene = Scene { elements, size: self.output_pixels(), clear: self.clear_colour() };
        // Rendered on first use, and shared by the captures read back from it.
        let mut texture = None;
        for capture in pending {
            let region = capture.region;
            match capture.target {
                CaptureTarget::File(path) => match pixels(renderer, &mut texture, &scene, region) {
                    Ok(rgba) => {
                        let (w, h) = (region.size.w as u32, region.size.h as u32);
                        match write_png(&path, w, h, &rgba) {
                            Ok(()) => tracing::info!("screenshot saved to {}", path.display()),
                            Err(err) => tracing::warn!("could not write {}: {err}", path.display()),
                        }
                    }
                    Err(err) => tracing::warn!("capture failed: {err}"),
                },
                CaptureTarget::Screencopy { frame, buffer, damage } => {
                    let copied = pixels(renderer, &mut texture, &scene, region)
                        .and_then(|rgba| copy_into_shm(&buffer, region.size.w, region.size.h, &rgba));
                    finish_frame(&frame, region.size, damage, copied);
                }
                CaptureTarget::Dmabuf { frame, mut dmabuf, damage } => {
                    let rendered = render_scene(renderer, &mut dmabuf, &scene);
                    finish_frame(&frame, region.size, damage, rendered);
                }
            }
        }
    }
}

/// Renders the scene into a texture or GPU buffer, and waits for the GPU to
/// finish so that it can be handed over.
fn render_scene<T>(renderer: &mut GlesRenderer, buffer: &mut T, scene: &Scene) -> Result<(), String>
where
    GlesRenderer: Bind<T>,
{
    let mut target = renderer.bind(buffer).map_err(|e| e.to_string())?;
    let mut damage = OutputDamageTracker::new(scene.size, 1.0, Transform::Normal);
    let result =
        damage.render_output(renderer, &mut target, 0, &scene.elements, scene.clear).map_err(|e| format!("{e:?}"))?;
    result.sync.wait().map_err(|e| format!("{e:?}"))
}

/// Reads a region of the scene as RGBA rows, rendering it into `texture` the
/// first time.
fn pixels(
    renderer: &mut GlesRenderer,
    texture: &mut Option<Result<GlesTexture, String>>,
    scene: &Scene,
    region: Rectangle<i32, Buffer>,
) -> Result<Vec<u8>, String> {
    let rendered = texture.get_or_insert_with(|| {
        let mut texture: GlesTexture =
            renderer.create_buffer(Fourcc::Abgr8888, scene.size.into()).map_err(|e| e.to_string())?;
        render_scene(renderer, &mut texture, scene)?;
        Ok(texture)
    });
    read_region(renderer, rendered.as_ref().map_err(Clone::clone)?, region)
}

/// Tells a screencopy client that its buffer is filled, or that it failed.
fn finish_frame(frame: &ZwlrScreencopyFrameV1, size: Size<i32, Buffer>, damage: bool, result: Result<(), String>) {
    if let Err(err) = result {
        tracing::warn!("screencopy: {err}");
        frame.failed();
        return;
    }
    frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
    if damage {
        frame.damage(0, 0, size.w as u32, size.h as u32);
    }
    let now: Duration = Clock::<Monotonic>::new().now().into();
    let secs = now.as_secs();
    frame.ready((secs >> 32) as u32, secs as u32, now.subsec_nanos());
}

/// Reads a region of a texture as RGBA rows, top to bottom.
fn read_region(
    renderer: &mut GlesRenderer,
    texture: &GlesTexture,
    region: Rectangle<i32, Buffer>,
) -> Result<Vec<u8>, String> {
    let mapping = renderer.copy_texture(texture, region, Fourcc::Abgr8888).map_err(|e| e.to_string())?;
    // GL keeps a texture's rows bottom-up; a texture we rendered into is not
    // marked flipped, but its rows still need reversing to read top-down.
    let flipped = !mapping.flipped();
    let data = renderer.map_texture(&mapping).map_err(|e| e.to_string())?;
    let row = region.size.w as usize * 4;
    let rows = region.size.h as usize;
    if data.len() < row * rows {
        return Err("short read".into());
    }
    let mut out = Vec::with_capacity(row * rows);
    for y in 0..rows {
        let src = if flipped { rows - 1 - y } else { y };
        out.extend_from_slice(&data[src * row..src * row + row]);
    }
    Ok(out)
}

/// Copies RGBA rows into a client's XRGB8888 shared memory buffer.
fn copy_into_shm(buffer: &WlBuffer, w: i32, h: i32, rgba: &[u8]) -> Result<(), String> {
    with_buffer_contents_mut(buffer, |ptr, len, info| {
        if info.width != w || info.height != h || info.stride < w * 4 {
            return Err(format!("buffer is {}x{}, expected {w}x{h}", info.width, info.height));
        }
        if !matches!(info.format, wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888) {
            return Err(format!("unsupported format {:?}", info.format));
        }
        let needed = info.offset as usize + info.stride as usize * h as usize;
        if needed > len {
            return Err("buffer too small".into());
        }
        // SAFETY: The range was checked against the pool's length above. Smithay
        // provides exclusive access to the mapped buffer for this callback.
        #[allow(unsafe_code, reason = "Smithay exposes the client's mapped SHM buffer as a raw pointer")]
        let dst =
            unsafe { std::slice::from_raw_parts_mut(ptr.add(info.offset as usize), needed - info.offset as usize) };
        for y in 0..h as usize {
            let src = &rgba[y * w as usize * 4..(y + 1) * w as usize * 4];
            let row = &mut dst[y * info.stride as usize..y * info.stride as usize + w as usize * 4];
            // RGBA bytes to little-endian XRGB, which is B G R X in memory.
            for (d, s) in row.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = 255;
            }
        }
        Ok(())
    })
    .map_err(|e| format!("{e:?}"))?
}

fn write_png(path: &PathBuf, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    // Opaque: the scene has no meaningful alpha.
    let opaque: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    writer.write_image_data(&opaque).map_err(|e| e.to_string())
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for Tileception {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for Tileception {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        let full = state.output_region();
        let (frame, region) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput { frame, .. } => (frame, full),
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion { frame, x, y, width, height, .. } => {
                // Asked for in logical pixels (slurp's), copied in physical ones.
                let r = crate::render::physical(
                    &tileception_core::Rect::new(x as f64, y as f64, width as f64, height as f64),
                    state.scale,
                );
                let asked = Rectangle::<i32, Buffer>::new((r.loc.x, r.loc.y).into(), (r.size.w, r.size.h).into());
                (frame, asked.intersection(full).unwrap_or(full))
            }
            _ => return,
        };
        let frame = init.init(frame, FrameData { region });
        let size = region.size;
        frame.buffer(wl_shm::Format::Xrgb8888, size.w as u32, size.h as u32, size.w as u32 * 4);
        if frame.version() >= 3 {
            // Screen sharing through xdg-desktop-portal-wlr requires a GPU
            // buffer on offer. Only whole outputs are rendered into one.
            if region == full {
                frame.linux_dmabuf(Fourcc::Xrgb8888 as u32, size.w as u32, size.h as u32);
            }
            frame.buffer_done();
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, FrameData> for Tileception {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &FrameData,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        let (buffer, damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            _ => return,
        };
        let target = if let Ok(dmabuf) = get_dmabuf(&buffer) {
            if dmabuf.size() != data.region.size {
                frame.failed();
                return;
            }
            CaptureTarget::Dmabuf { frame: frame.clone(), dmabuf: dmabuf.clone(), damage }
        } else if with_buffer_contents(&buffer, |_, _, _| ()).is_ok() {
            CaptureTarget::Screencopy { frame: frame.clone(), buffer, damage }
        } else {
            frame.failed();
            return;
        };
        state.pending_captures.push(PendingCapture { region: data.region, target });
        // With damage, the copy waits for the screen to change; without, it
        // is taken right away.
        if !damage {
            state.schedule_render();
        }
    }
}

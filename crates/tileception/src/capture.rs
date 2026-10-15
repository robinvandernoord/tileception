//! Screen capture: wlr-screencopy for grim, slurp and screen sharing through
//! xdg-desktop-portal-wlr, and `tileception msg screenshot <file.png>`.
//!
//! Captures are queued and taken on the next frame, by rendering the scene
//! into an off-screen texture and reading it back into the client's shared
//! memory buffer (or a PNG file).

use std::{path::PathBuf, time::Duration};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker, gles::GlesRenderer, gles::GlesTexture, Bind, ExportMem, Offscreen,
            TextureMapping,
        },
    },
    reexports::{
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
        },
        wayland_server::{
            protocol::{wl_buffer::WlBuffer, wl_shm},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Buffer, Clock, Monotonic, Rectangle, Transform},
    wayland::shm::{with_buffer_contents, with_buffer_contents_mut},
};

use crate::{render::BACKGROUND, Tileception};

/// Where a finished capture goes.
pub enum CaptureTarget {
    File(PathBuf),
    Screencopy { frame: ZwlrScreencopyFrameV1, buffer: WlBuffer, damage: bool },
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
    fn output_region(&self) -> Rectangle<i32, Buffer> {
        let r = self.output_rect();
        Rectangle::from_size((r.w as i32, r.h as i32).into())
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
        let image = match self.render_offscreen(renderer) {
            Ok(texture) => Some(texture),
            Err(err) => {
                tracing::warn!("capture failed: {err}");
                None
            }
        };
        for capture in pending {
            let pixels = image.as_ref().map(|t| read_region(renderer, t, capture.region));
            match (capture.target, pixels) {
                (CaptureTarget::File(path), Some(Ok(rgba))) => {
                    let (w, h) = (capture.region.size.w as u32, capture.region.size.h as u32);
                    match write_png(&path, w, h, &rgba) {
                        Ok(()) => tracing::info!("screenshot saved to {}", path.display()),
                        Err(err) => tracing::warn!("could not write {}: {err}", path.display()),
                    }
                }
                (CaptureTarget::Screencopy { frame, buffer, damage }, Some(Ok(rgba))) => {
                    let size = capture.region.size;
                    match copy_into_shm(&buffer, size.w, size.h, &rgba) {
                        Ok(()) => {
                            frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
                            if damage {
                                frame.damage(0, 0, size.w as u32, size.h as u32);
                            }
                            let now: Duration = Clock::<Monotonic>::new().now().into();
                            let secs = now.as_secs();
                            frame.ready((secs >> 32) as u32, secs as u32, now.subsec_nanos());
                        }
                        Err(err) => {
                            tracing::warn!("screencopy: {err}");
                            frame.failed();
                        }
                    }
                }
                (target, result) => {
                    if let Some(Err(err)) = result {
                        tracing::warn!("capture failed: {err}");
                    }
                    if let CaptureTarget::Screencopy { frame, .. } = target {
                        frame.failed();
                    }
                }
            }
        }
    }

    /// Renders what is on screen, without the pointer, into a texture.
    fn render_offscreen(&self, renderer: &mut GlesRenderer) -> Result<GlesTexture, String> {
        let r = self.output_rect();
        let (w, h) = (r.w as i32, r.h as i32);
        let mut texture: GlesTexture =
            renderer.create_buffer(Fourcc::Abgr8888, (w, h).into()).map_err(|e| e.to_string())?;
        let elements = self.render_elements(renderer, self.now());
        let mut target = renderer.bind(&mut texture).map_err(|e| e.to_string())?;
        let mut damage = OutputDamageTracker::new((w, h), 1.0, Transform::Normal);
        damage.render_output(renderer, &mut target, 0, &elements, BACKGROUND).map_err(|e| format!("{e:?}"))?;
        drop(target);
        Ok(texture)
    }
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
        // Safety: the range was checked against the pool's length above.
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
                let asked = Rectangle::<i32, Buffer>::new((x, y).into(), (width, height).into());
                (frame, asked.intersection(full).unwrap_or(full))
            }
            _ => return,
        };
        let frame = init.init(frame, FrameData { region });
        let size = region.size;
        frame.buffer(wl_shm::Format::Xrgb8888, size.w as u32, size.h as u32, size.w as u32 * 4);
        if frame.version() >= 3 {
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
        if with_buffer_contents(&buffer, |_, _, _| ()).is_err() {
            // Only shared memory buffers are offered.
            frame.failed();
            return;
        }
        state.pending_captures.push(PendingCapture {
            region: data.region,
            target: CaptureTarget::Screencopy { frame: frame.clone(), buffer, damage },
        });
    }
}

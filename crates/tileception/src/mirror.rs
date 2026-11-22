//! Frames for outputs on a secondary GPU. The primary renderer draws them and
//! hands them over in a buffer both GPUs can use, or through memory when the
//! GPUs share no buffer. Only what changed since the last frame is drawn and
//! handed over, so a still screen costs nothing.

use smithay::{
    backend::{
        allocator::{
            Allocator, Fourcc, Modifier,
            dmabuf::{AsDmabuf, Dmabuf},
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::DrmDeviceFd,
        renderer::{
            Bind, ImportDma, ImportMem, Offscreen,
            element::{
                Kind,
                texture::{TextureRenderBuffer, TextureRenderElement},
            },
            gles::{GlesRenderer, GlesTexture},
        },
    },
    utils::{Buffer, Physical, Rectangle, Size, Transform},
};

use crate::{
    capture::{Redraw, patch_rows},
    render::OutputElement,
};

pub struct Mirror {
    /// What changed since the last frame, on the primary renderer.
    redraw: Redraw,
    /// The frame on the secondary GPU.
    shown: TextureRenderBuffer<GlesTexture>,
    handover: Handover,
}

enum Handover {
    /// Drawn by the primary, sampled by the secondary GPU.
    Shared(Dmabuf),
    /// Drawn on the primary, read back into `pixels` and uploaded from there.
    Memory { texture: GlesTexture, pixels: Vec<u8> },
}

impl Mirror {
    pub fn new(
        gbm: &GbmDevice<DrmDeviceFd>,
        primary: &mut GlesRenderer,
        secondary: &mut GlesRenderer,
        size: (i32, i32),
    ) -> Result<Self, String> {
        let size = Size::from(size);
        let (handover, texture) = match shared(gbm, primary, secondary, size) {
            Ok(shared) => shared,
            Err(error) => {
                tracing::info!("the GPUs share no buffer ({error}); copying frames through memory");
                memory(primary, secondary, size)?
            }
        };
        let full = Rectangle::from_size((size.w, size.h).into());
        Ok(Self {
            redraw: Redraw::new((size.w, size.h)),
            shown: TextureRenderBuffer::from_texture(secondary, texture, 1, Transform::Normal, Some(vec![full])),
            handover,
        })
    }

    pub fn size(&self) -> (i32, i32) {
        self.redraw.size()
    }

    /// Draws what changed of `elements` and hands it to the secondary GPU,
    /// returning the element that shows the frame there.
    pub fn render(
        &mut self,
        primary: &mut GlesRenderer,
        secondary: &mut GlesRenderer,
        elements: &[OutputElement],
        clear: [f32; 4],
    ) -> Result<OutputElement, String> {
        let (width, height) = self.redraw.size();
        let full = Rectangle::from_size((width, height).into());
        match &mut self.handover {
            Handover::Shared(dmabuf) => {
                let damage = self.redraw.draw(primary, dmabuf, elements, clear)?;
                if let Err(error) = self.shown.render().draw(|_| Ok::<_, String>(damage)) {
                    self.redraw.forget();
                    return Err(error);
                }
            }
            Handover::Memory { texture, pixels } => {
                let damage = self.redraw.draw(primary, texture, elements, clear)?;
                for rect in damage {
                    let handed = crate::capture::read_region(primary, texture, rect)
                        .and_then(|rows| {
                            patch_rows(pixels, width, rect, &rows)
                                .then_some(())
                                .ok_or_else(|| format!("{rect:?} is not inside the {width}x{height} frame"))
                        })
                        .and_then(|()| {
                            self.shown
                                .update_from_memory(secondary, pixels, rect, Some(vec![full]))
                                .map_err(|error| error.to_string())
                        });
                    if let Err(error) = handed {
                        self.redraw.forget();
                        return Err(error);
                    }
                }
            }
        }
        Ok(OutputElement::Texture(TextureRenderElement::from_texture_render_buffer(
            (0.0, 0.0),
            &self.shown,
            None,
            None,
            None,
            Kind::Unspecified,
        )))
    }
}

/// A buffer on the primary GPU, in a layout the secondary can sample.
fn shared(
    gbm: &GbmDevice<DrmDeviceFd>,
    primary: &mut GlesRenderer,
    secondary: &mut GlesRenderer,
    size: Size<i32, Physical>,
) -> Result<(Handover, GlesTexture), String> {
    let sampled = ImportDma::dmabuf_formats(secondary);
    let modifiers: Vec<Modifier> = primary
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .filter(|format| format.code == Fourcc::Abgr8888 && sampled.contains(format))
        .map(|format| format.modifier)
        .collect();
    if modifiers.is_empty() {
        return Err("no buffer layout both can use".into());
    }
    let buffer = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING)
        .create_buffer(size.w as u32, size.h as u32, Fourcc::Abgr8888, &modifiers)
        .map_err(|error| error.to_string())?;
    let mut dmabuf = buffer.export().map_err(|error| error.to_string())?;
    // Drivers may still refuse what they listed, and log it as a GL error:
    // expected here, as the copy through memory takes over.
    crate::bar::expecting_errors(|| {
        // Fails early if the primary cannot draw into it.
        primary.bind(&mut dmabuf).map_err(|error| error.to_string())?;
        secondary.import_dmabuf(&dmabuf, None).map_err(|error| error.to_string())
    })
    .map(|texture| (Handover::Shared(dmabuf), texture))
}

fn memory(
    primary: &mut GlesRenderer,
    secondary: &mut GlesRenderer,
    size: Size<i32, Physical>,
) -> Result<(Handover, GlesTexture), String> {
    let buffer_size = Size::<i32, Buffer>::from((size.w, size.h));
    let texture: GlesTexture =
        primary.create_buffer(Fourcc::Abgr8888, buffer_size).map_err(|error| error.to_string())?;
    let pixels = vec![0; size.w as usize * size.h as usize * 4];
    let shown =
        secondary.import_memory(&pixels, Fourcc::Abgr8888, buffer_size, false).map_err(|error| error.to_string())?;
    Ok((Handover::Memory { texture, pixels }, shown))
}

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
            damage::OutputDamageTracker,
            element::{
                Kind,
                texture::{TextureRenderBuffer, TextureRenderElement},
            },
            gles::{GlesRenderer, GlesTexture},
        },
    },
    utils::{Buffer, Physical, Rectangle, Size, Transform},
};

use crate::render::OutputElement;

pub struct Mirror {
    size: Size<i32, Physical>,
    /// What changed since the last frame, on the primary renderer.
    damage: OutputDamageTracker,
    /// The frame on the secondary GPU.
    shown: TextureRenderBuffer<GlesTexture>,
    handover: Handover,
    /// The primary's buffer holds the last frame, so only damage needs drawing.
    drawn: bool,
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
            size,
            damage: OutputDamageTracker::new(size, 1.0, Transform::Normal),
            shown: TextureRenderBuffer::from_texture(secondary, texture, 1, Transform::Normal, Some(vec![full])),
            handover,
            drawn: false,
        })
    }

    pub fn size(&self) -> (i32, i32) {
        (self.size.w, self.size.h)
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
        let age = usize::from(std::mem::take(&mut self.drawn));
        let full = Rectangle::from_size(self.size);
        match &mut self.handover {
            Handover::Shared(dmabuf) => {
                let damage = draw(primary, dmabuf, &mut self.damage, age, elements, clear)?;
                let damage = damage.iter().filter_map(|rect| rect.intersection(full)).map(buffer_rect).collect();
                self.shown.render().draw(|_| Ok::<_, String>(damage))?;
            }
            Handover::Memory { texture, pixels } => {
                let damage = draw(primary, texture, &mut self.damage, age, elements, clear)?;
                let stride = self.size.w as usize * 4;
                for rect in damage.iter().filter_map(|rect| rect.intersection(full)).map(buffer_rect) {
                    let rows = crate::capture::read_region(primary, texture, rect)?;
                    let width = rect.size.w as usize * 4;
                    for (line, row) in rows.chunks_exact(width).enumerate() {
                        let start = (rect.loc.y as usize + line) * stride + rect.loc.x as usize * 4;
                        pixels[start..start + width].copy_from_slice(row);
                    }
                    self.shown
                        .update_from_memory(secondary, pixels, rect, Some(vec![buffer_rect(full)]))
                        .map_err(|error| error.to_string())?;
                }
            }
        }
        self.drawn = true;
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

/// A linear buffer on the primary GPU, which the secondary can sample.
fn shared(
    gbm: &GbmDevice<DrmDeviceFd>,
    primary: &mut GlesRenderer,
    secondary: &mut GlesRenderer,
    size: Size<i32, Physical>,
) -> Result<(Handover, GlesTexture), String> {
    let buffer = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING)
        .create_buffer(size.w as u32, size.h as u32, Fourcc::Abgr8888, &[Modifier::Linear])
        .map_err(|error| error.to_string())?;
    let mut dmabuf = buffer.export().map_err(|error| error.to_string())?;
    // Fails early if the primary cannot draw into it.
    primary.bind(&mut dmabuf).map_err(|error| error.to_string())?;
    let texture = secondary.import_dmabuf(&dmabuf, None).map_err(|error| error.to_string())?;
    Ok((Handover::Shared(dmabuf), texture))
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

/// Draws what changed since the buffer's last frame, returning where.
fn draw<T>(
    primary: &mut GlesRenderer,
    buffer: &mut T,
    tracker: &mut OutputDamageTracker,
    age: usize,
    elements: &[OutputElement],
    clear: [f32; 4],
) -> Result<Vec<Rectangle<i32, Physical>>, String>
where
    GlesRenderer: Bind<T>,
{
    let mut target = primary.bind(buffer).map_err(|error| error.to_string())?;
    let result =
        tracker.render_output(primary, &mut target, age, elements, clear).map_err(|error| format!("{error:?}"))?;
    let damage = result.damage.cloned().unwrap_or_default();
    // The secondary GPU reads the frame next, so the primary must finish it.
    result.sync.wait().map_err(|error| format!("{error:?}"))?;
    Ok(damage)
}

fn buffer_rect(rect: Rectangle<i32, Physical>) -> Rectangle<i32, Buffer> {
    Rectangle::new((rect.loc.x, rect.loc.y).into(), (rect.size.w, rect.size.h).into())
}

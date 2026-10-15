//! Shared text and icon drawing for the compositor's interface.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
};

use smithay::input::keyboard::xkb;
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{element::memory::MemoryRenderBuffer, gles::GlesRenderer},
    },
    utils::Transform,
};
use tileception_core::Rect;
use zeroize::Zeroizing;

use crate::{
    render::{push_image, OutputElement},
    theme::Colour,
    Tileception,
};

#[derive(Default)]
pub struct Icons(RefCell<HashMap<(PathBuf, u32), Option<MemoryRenderBuffer>>>);

pub fn compose_state() -> Option<xkb::compose::State> {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|key| std::env::var_os(key).filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "C.UTF-8".into());
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let table = xkb::compose::Table::new_from_locale(&context, &locale, xkb::compose::COMPILE_NO_FLAGS).ok()?;
    Some(xkb::compose::State::new(&table, xkb::compose::STATE_NO_FLAGS))
}

pub fn compose_text(compose: &mut Option<xkb::compose::State>, sym: u32) -> Option<Zeroizing<String>> {
    if let Some(state) = compose {
        state.feed(sym.into());
        match state.status() {
            xkb::compose::Status::Composing => return None,
            xkb::compose::Status::Cancelled => {
                state.reset();
                return None;
            }
            xkb::compose::Status::Composed => {
                let text = state.utf8().map(Zeroizing::new);
                state.reset();
                return text;
            }
            xkb::compose::Status::Nothing => {}
        }
    }
    char::from_u32(xkb::keysym_to_utf32(sym.into()))
        .filter(|ch| !ch.is_control())
        .map(|ch| Zeroizing::new(ch.to_string()))
}

impl Icons {
    pub fn image(&self, path: &Path, size: u32) -> Option<MemoryRenderBuffer> {
        let key = (path.to_path_buf(), size);
        if let Some(image) = self.0.borrow().get(&key) {
            return image.clone();
        }
        let image = load_icon(path, size);
        let mut cache = self.0.borrow_mut();
        if cache.len() >= 512 {
            cache.clear();
        }
        cache.insert(key, image.clone());
        image
    }
}

fn load_icon(path: &Path, size: u32) -> Option<MemoryRenderBuffer> {
    let pixels = if path.extension().is_some_and(|e| e == "svg") {
        let data = std::fs::read(path).ok()?;
        let options =
            resvg::usvg::Options { resources_dir: path.parent().map(Path::to_path_buf), ..Default::default() };
        let tree = resvg::usvg::Tree::from_data(&data, &options).ok()?;
        let mut pixels = resvg::tiny_skia::Pixmap::new(size, size)?;
        let scale = (size as f32 / tree.size().width()).min(size as f32 / tree.size().height());
        let transform = resvg::tiny_skia::Transform::from_translate(
            (size as f32 - tree.size().width() * scale) / 2.0,
            (size as f32 - tree.size().height() * scale) / 2.0,
        )
        .pre_scale(scale, scale);
        resvg::render(&tree, transform, &mut pixels.as_mut());
        pixels.take()
    } else {
        let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
        let image = reader.decode().ok()?.resize(size, size, image::imageops::FilterType::Triangle).to_rgba8();
        let mut canvas = image::RgbaImage::new(size, size);
        image::imageops::overlay(
            &mut canvas,
            &image,
            ((size - image.width()) / 2) as i64,
            ((size - image.height()) / 2) as i64,
        );
        for pixel in canvas.pixels_mut() {
            let alpha = pixel.0[3] as u32;
            for channel in &mut pixel.0[..3] {
                *channel = (*channel as u32 * alpha / 255) as u8;
            }
        }
        canvas.into_raw()
    };
    Some(MemoryRenderBuffer::from_slice(
        &pixels,
        Fourcc::Abgr8888,
        (size as i32, size as i32),
        1,
        Transform::Normal,
        None,
    ))
}

impl Tileception {
    pub fn ui_label(
        &self,
        out: &mut Vec<OutputElement>,
        renderer: &mut GlesRenderer,
        label: &str,
        rect: Rect,
        colour: Colour,
        bold: bool,
    ) {
        if let Some(image) = self.text.borrow_mut().line(
            label,
            &self.theme.font,
            self.theme.font_size,
            colour,
            rect.w.max(1.0) as f32,
            bold,
        ) {
            push_image(out, renderer, &image.buffer, (rect.x, rect.y + (rect.h - image.height as f64) / 2.0));
        }
    }
}

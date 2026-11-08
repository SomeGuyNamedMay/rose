//! Named cursors, which a host desktop draws for Rose when it's nested but Rose has to draw itself
//! on the hardware. They come from the XCursor theme, with a built-in arrow to fall back on.

use std::collections::HashMap;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind,
            },
            gles::GlesRenderer,
        },
    },
    input::pointer::CursorIcon,
    utils::{Logical, Point, Transform},
};
use xcursor::{parser::parse_xcursor, CursorTheme};

use crate::render::RoseRenderElement;

struct Image {
    buffer: MemoryRenderBuffer,
    hotspot: Point<i32, Logical>,
}

impl Image {
    fn new(pixels: &[u8], size: (i32, i32), hotspot: (i32, i32)) -> Self {
        Self {
            buffer: MemoryRenderBuffer::from_slice(
                pixels,
                Fourcc::Abgr8888,
                size,
                1,
                Transform::Normal,
                None,
            ),
            hotspot: hotspot.into(),
        }
    }
}

pub struct Cursors {
    theme: CursorTheme,
    size: u32,
    images: HashMap<CursorIcon, Image>,
}

impl Cursors {
    pub fn new() -> Self {
        let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into());
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(24);
        Self {
            theme: CursorTheme::load(&theme),
            size,
            images: HashMap::new(),
        }
    }

    fn load(&self, icon: CursorIcon) -> Option<Image> {
        let path = std::iter::once(icon.name())
            .chain(icon.alt_names().iter().copied())
            .chain(std::iter::once("default"))
            .find_map(|name| self.theme.load_icon(name))?;
        let images = parse_xcursor(&std::fs::read(path).ok()?)?;
        // Animated cursors show their first frame.
        let image = images
            .iter()
            .min_by_key(|image| image.size.abs_diff(self.size))?;
        Some(Image::new(
            &image.pixels_rgba,
            (image.width as i32, image.height as i32),
            (image.xhot as i32, image.yhot as i32),
        ))
    }

    pub fn element(
        &mut self,
        renderer: &mut GlesRenderer,
        icon: CursorIcon,
        location: Point<i32, Logical>,
    ) -> Option<RoseRenderElement> {
        if !self.images.contains_key(&icon) {
            let image = self.load(icon).unwrap_or_else(arrow);
            self.images.insert(icon, image);
        }
        let image = &self.images[&icon];
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (location - image.hotspot).to_f64().to_physical(1.0),
            &image.buffer,
            None,
            None,
            None,
            Kind::Cursor,
        )
        .map_err(|err| tracing::warn!("failed to upload the cursor: {err:?}"))
        .ok()
        .map(RoseRenderElement::from)
    }
}

/// A white arrow with a black edge, for when no cursor theme is installed.
fn arrow() -> Image {
    const WIDTH: usize = 12;
    const HEIGHT: usize = 18;
    let mut pixels = vec![0; WIDTH * HEIGHT * 4];
    for y in 0..HEIGHT {
        let right = y * 3 / 5;
        for x in 0..=right {
            let edge = x == 0 || x == right || y == HEIGHT - 1;
            let shade = if edge { 0x00 } else { 0xFF };
            pixels[(y * WIDTH + x) * 4..][..4].copy_from_slice(&[shade, shade, shade, 0xFF]);
        }
    }
    Image::new(&pixels, (WIDTH as i32, HEIGHT as i32), (0, 0))
}

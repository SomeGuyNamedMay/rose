//! Software text and shape drawing for the bar and menu, into ARGB8888 memory buffers.

use std::collections::HashMap;

use fontdue::{Font as FontData, FontSettings, Metrics};

use crate::config::Color;

pub struct Font {
    font: FontData,
    size: f32,
    glyphs: HashMap<char, (Metrics, Vec<u8>)>,
    /// Pixels from the top of a line to the baseline.
    pub ascent: i32,
    pub line_height: i32,
}

impl Font {
    /// Loads a font by family name (`monospace`, `sans-serif` and `serif` follow fontconfig), or
    /// from a file if `family` is a path. Falls back to any monospaced font.
    pub fn load(family: &str, size: f32) -> Result<Self, String> {
        let (data, index) = if family.contains('/') {
            let data = std::fs::read(family).map_err(|e| format!("{family}: {e}"))?;
            (data, 0)
        } else {
            find_font(family).ok_or("no fonts found")?
        };
        let font = FontData::from_bytes(
            data,
            FontSettings {
                collection_index: index,
                scale: size,
                ..FontSettings::default()
            },
        )
        .map_err(|e| format!("{family}: {e}"))?;

        let (ascent, descent) = font
            .horizontal_line_metrics(size)
            .map_or((size * 0.8, -size * 0.2), |m| (m.ascent, m.descent));
        Ok(Self {
            font,
            size,
            glyphs: HashMap::new(),
            ascent: ascent.round() as i32,
            line_height: (ascent - descent).ceil() as i32,
        })
    }

    fn glyph(&mut self, c: char) -> &(Metrics, Vec<u8>) {
        let (font, size) = (&self.font, self.size);
        self.glyphs
            .entry(c)
            .or_insert_with(|| font.rasterize(c, size))
    }

    fn advance(&mut self, c: char) -> f32 {
        self.glyph(c).0.advance_width
    }

    pub fn width(&mut self, text: &str) -> i32 {
        text.chars().map(|c| self.advance(c)).sum::<f32>().ceil() as i32
    }

    /// The longest prefix of `text` that fits in `max` pixels, ending in `…` if it was cut.
    pub fn truncate(&mut self, text: &str, max: i32) -> String {
        if self.width(text) <= max {
            return text.to_string();
        }
        let budget = max as f32 - self.advance('…');
        let mut width = 0.0;
        let mut out = String::new();
        for c in text.chars() {
            width += self.advance(c);
            if width > budget {
                break;
            }
            out.push(c);
        }
        out.push('…');
        out
    }
}

fn find_font(family: &str) -> Option<(Vec<u8>, u32)> {
    use fontdb::{Family, Query, Stretch, Style, Weight};

    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let family = match family {
        "monospace" => Family::Monospace,
        "sans-serif" | "sans" => Family::SansSerif,
        "serif" => Family::Serif,
        name => Family::Name(name),
    };
    let regular = |f: &&fontdb::FaceInfo| {
        f.style == Style::Normal && f.weight == Weight::NORMAL && f.stretch == Stretch::Normal
    };
    let id = db
        .query(&Query {
            families: &[family],
            ..Query::default()
        })
        .or_else(|| {
            tracing::warn!("font {family:?} not found, using another monospaced font");
            let mut faces: Vec<_> = db.faces().filter(|f| f.monospaced).collect();
            faces.sort_by_key(|f| !regular(f));
            faces.first().map(|f| f.id)
        })
        .or_else(|| db.faces().next().map(|f| f.id))?;
    db.with_face_data(id, |data, index| (data.to_vec(), index))
}

/// A pixel rectangle: x, y, width and height.
pub type Rect = (i32, i32, i32, i32);

/// Draws into an ARGB8888 buffer.
pub struct Canvas<'a> {
    pub data: &'a mut [u8],
    pub width: i32,
    pub height: i32,
}

fn rgb(color: Color) -> [f32; 3] {
    [color.0[0], color.0[1], color.0[2]]
}

impl Canvas<'_> {
    /// Blends `color` into a pixel with coverage `alpha`.
    fn blend(&mut self, x: i32, y: i32, color: [f32; 3], alpha: f32) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height || alpha <= 0.0 {
            return;
        }
        let i = ((y * self.width + x) * 4) as usize;
        let px = &mut self.data[i..i + 4];
        // Bytes are B, G, R, A.
        for (byte, channel) in [(2, 0), (1, 1), (0, 2)] {
            let old = px[byte] as f32 / 255.0;
            let new = old + (color[channel] - old) * alpha.min(1.0);
            px[byte] = (new * 255.0).round() as u8;
        }
        px[3] = 0xFF;
    }

    pub fn fill(&mut self, (x, y, w, h): Rect, color: Color) {
        let color = rgb(color);
        for row in y.max(0)..(y + h).min(self.height) {
            for col in x.max(0)..(x + w).min(self.width) {
                self.blend(col, row, color, 1.0);
            }
        }
    }

    /// Fills rows `y..y + h` between two edges that lean right towards the top, like `/`. Each
    /// edge is at `x` on the bottom row and at `x + slant` on the top row; a slant of 0 is upright.
    pub fn fill_slanted(
        &mut self,
        (left, left_slant): (i32, i32),
        (right, right_slant): (i32, i32),
        (y, h): (i32, i32),
        color: Color,
    ) {
        let color = rgb(color);
        for row in 0..h {
            // How far up the row's center is, from 0 at the bottom to 1 at the top.
            let t = 1.0 - (row as f32 + 0.5) / h as f32;
            let l = left as f32 + left_slant as f32 * t;
            let r = right as f32 + right_slant as f32 * t;
            for col in l.floor() as i32..r.ceil() as i32 {
                let (c0, c1) = (col as f32, col as f32 + 1.0);
                let coverage = (c1.min(r) - c0.max(l)).clamp(0.0, 1.0);
                self.blend(col, y + row, color, coverage);
            }
        }
    }

    /// Draws `text` with its line's top at `y`, returning the x where it ends.
    pub fn text(&mut self, font: &mut Font, x: i32, y: i32, text: &str, color: Color) -> i32 {
        self.text_with(font, x, y, text, |_| color)
    }

    /// Like `text`, choosing each character's color by its index.
    pub fn text_with(
        &mut self,
        font: &mut Font,
        x: i32,
        y: i32,
        text: &str,
        color: impl Fn(usize) -> Color,
    ) -> i32 {
        let baseline = y + font.ascent;
        let mut pen = x as f32;
        for (i, c) in text.chars().enumerate() {
            let rgb = rgb(color(i));
            let (metrics, bitmap) = font.glyph(c);
            let gx = pen.round() as i32 + metrics.xmin;
            let gy = baseline - metrics.height as i32 - metrics.ymin;
            let (gw, advance) = (metrics.width, metrics.advance_width);
            for (j, coverage) in bitmap.iter().enumerate() {
                if *coverage > 0 {
                    let (col, row) = ((j % gw) as i32, (j / gw) as i32);
                    // A little gamma keeps light text on dark backgrounds from looking thin.
                    let alpha = (*coverage as f32 / 255.0).powf(0.8);
                    self.blend(gx + col, gy + row, rgb, alpha);
                }
            }
            pen += advance;
        }
        pen.ceil() as i32
    }
}

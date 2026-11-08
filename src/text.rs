//! Text and shape drawing for the bar and menu, with egui.

use std::sync::Arc;

use egui::{
    epaint::{text::FontData, textures::TexturesDelta, ClippedPrimitive, Stroke},
    pos2,
    text::LayoutJob,
    vec2, Color32, Context, FontDefinitions, FontFamily, FontId, LayerId, Painter, Pos2, RawInput,
    Rect as EguiRect, Shape, TextFormat,
};
use egui_glow::glow;
use smithay::backend::{egl, renderer::gles::GlesRenderer};

use crate::config::Color;

/// A font, and the egui context it is laid out and drawn with.
pub struct Font {
    context: Context,
    id: FontId,
    /// Font atlas updates that the GL painter hasn't received yet.
    textures: TexturesDelta,
    pub line_height: i32,
}

/// One pass of egui, ready to be drawn with OpenGL.
pub struct Drawn {
    pub primitives: Vec<ClippedPrimitive>,
    pub textures: TexturesDelta,
}

fn raw_input((width, height): (i32, i32)) -> RawInput {
    RawInput {
        screen_rect: Some(EguiRect::from_min_size(
            Pos2::ZERO,
            vec2(width as f32, height as f32),
        )),
        ..RawInput::default()
    }
}

impl Font {
    /// Loads a font by family name (`monospace`, `sans-serif` and `serif` follow fontconfig), or
    /// from a file if `family` is a path. Falls back to any monospaced font.
    pub fn load(family: &str, size: f32) -> Result<Self, String> {
        let (data, index) = if family.contains('/') {
            let data = std::fs::read(family).map_err(|e| format!("{family}: {e}"))?;
            let mut db = fontdb::Database::new();
            db.load_font_data(data.clone());
            if db.is_empty() {
                return Err(format!("{family}: not a font file"));
            }
            (data, 0)
        } else {
            find_font(family).ok_or("no fonts found")?
        };

        let mut definitions = FontDefinitions::empty();
        let mut font = FontData::from_owned(data);
        font.index = index;
        definitions.font_data.insert("rose".into(), Arc::new(font));
        for family in [FontFamily::Monospace, FontFamily::Proportional] {
            definitions.families.insert(family, vec!["rose".into()]);
        }
        let context = Context::default();
        context.set_fonts(definitions);

        // egui only has fonts once a pass has begun.
        let id = FontId::monospace(size);
        context.begin_pass(raw_input((1, 1)));
        let line_height = context.fonts_mut(|f| f.row_height(&id)).ceil() as i32;
        let textures = context.end_pass().textures_delta;
        Ok(Self {
            context,
            id,
            textures,
            line_height,
        })
    }

    pub fn width(&mut self, text: &str) -> i32 {
        let id = &self.id;
        self.context
            .fonts_mut(|f| text.chars().map(|c| f.glyph_width(id, c)).sum::<f32>())
            .ceil() as i32
    }

    /// The longest prefix of `text` that fits in `max` pixels, ending in `…` if it was cut.
    pub fn truncate(&mut self, text: &str, max: i32) -> String {
        if self.width(text) <= max {
            return text.to_string();
        }
        let id = &self.id;
        self.context.fonts_mut(|f| {
            let budget = max as f32 - f.glyph_width(id, '…');
            let mut width = 0.0;
            let mut out = String::new();
            for c in text.chars() {
                width += f.glyph_width(id, c);
                if width > budget {
                    break;
                }
                out.push(c);
            }
            out.push('…');
            out
        })
    }

    /// Runs a pass of egui over a surface of `size`, with `paint` doing the drawing.
    pub fn draw(&mut self, size: (i32, i32), paint: impl FnOnce(&mut Canvas, &mut Font)) -> Drawn {
        self.context.begin_pass(raw_input(size));
        let mut canvas = Canvas {
            painter: self.context.layer_painter(LayerId::background()),
            width: size.0,
            height: size.1,
        };
        paint(&mut canvas, self);
        let output = self.context.end_pass();

        let mut textures = std::mem::take(&mut self.textures);
        textures.append(output.textures_delta);
        Drawn {
            primitives: self
                .context
                .tessellate(output.shapes, output.pixels_per_point),
            textures,
        }
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        // egui asserts that updates are applied, but these have nowhere to go.
        self.textures.clear();
    }
}

/// Creates the painter that draws egui's output, on the renderer's OpenGL context.
pub fn gl_painter(renderer: &mut GlesRenderer) -> Result<egui_glow::Painter, String> {
    renderer
        .with_context(|_| {
            // Safety: the renderer's context is current, and outlives the painter.
            let gl =
                unsafe { glow::Context::from_loader_function(|name| egl::get_proc_address(name)) };
            egui_glow::Painter::new(Arc::new(gl), "", None, false).map_err(|e| e.to_string())
        })
        .map_err(|e| e.to_string())?
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

/// Draws into a pass of egui.
pub struct Canvas {
    painter: Painter,
    pub width: i32,
    pub height: i32,
}

fn color32(color: Color) -> Color32 {
    let channel = |c: f32| (c * 255.0).round() as u8;
    Color32::from_rgb(
        channel(color.0[0]),
        channel(color.0[1]),
        channel(color.0[2]),
    )
}

impl Canvas {
    pub fn fill(&mut self, (x, y, w, h): Rect, color: Color) {
        if w > 0 && h > 0 {
            let min = pos2(x as f32, y as f32);
            let rect = EguiRect::from_min_size(min, vec2(w as f32, h as f32));
            self.painter.rect_filled(rect, 0.0, color32(color));
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
        let (top, bottom) = (y as f32, (y + h) as f32);
        let points = vec![
            pos2((left + left_slant) as f32, top),
            pos2((right + right_slant) as f32, top),
            pos2(right as f32, bottom),
            pos2(left as f32, bottom),
        ];
        self.painter
            .add(Shape::convex_polygon(points, color32(color), Stroke::NONE));
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
        let mut job = LayoutJob {
            break_on_newline: false,
            ..LayoutJob::default()
        };
        let format = |color| TextFormat::simple(font.id.clone(), color);
        let mut run = String::new();
        let mut run_color = None;
        for (i, c) in text.chars().enumerate() {
            let color = color32(color(i));
            if run_color.is_some_and(|last| last != color) {
                job.append(&run, 0.0, format(run_color.unwrap()));
                run.clear();
            }
            run_color = Some(color);
            run.push(c);
        }
        let Some(last) = run_color else {
            return x;
        };
        job.append(&run, 0.0, format(last));

        let galley = font.context.fonts_mut(|f| f.layout_job(job));
        let end = x as f32 + galley.size().x;
        self.painter
            .galley(pos2(x as f32, y as f32), galley, Color32::WHITE);
        end.ceil() as i32
    }
}

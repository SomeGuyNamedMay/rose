//! The bar, styled after Nyxt's status buffer: slanted segments for the sheets, the focused view
//! and the mode, with the other visible views listed like tabs. The menu opens next to it.

use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::memory::MemoryRenderBufferRenderElement,
            element::{memory::MemoryRenderBuffer, Kind},
            gles::GlesRenderer,
        },
    },
    desktop::Window,
    reexports::calloop::channel::Sender,
    utils::{Logical, Point, Rectangle, Transform},
};

use crate::{
    config::{BarConfig, BarPosition, Color, Theme},
    menu::Menu,
    render::RoseRenderElement,
    text::{Canvas, Font},
    wm::{self, Mode},
    Rose,
};

/// What clicking part of the bar does.
#[derive(Clone)]
enum Target {
    Sheet(u8),
    View(Window),
}

/// Everything the bar shows, so it is only repainted when this changes.
#[derive(Clone, PartialEq)]
struct Content {
    size: (i32, i32),
    theme: Theme,
    /// Inhabited sheets, and whether each is the current one.
    sheets: Vec<(u8, bool)>,
    title: String,
    tabs: Vec<(String, Window)>,
    mode: String,
    /// Replaces the mode segment's color while a mode waits for input.
    mode_color: Option<Color>,
    status: String,
}

/// A memory buffer and the size it was created with.
#[derive(Default)]
struct Surface {
    buffer: Option<MemoryRenderBuffer>,
    size: (i32, i32),
}

impl Surface {
    /// Paints with `paint` into a buffer of `size`, recreating the buffer if the size changed.
    fn paint(&mut self, size: (i32, i32), paint: impl FnOnce(&mut Canvas)) {
        let full = Rectangle::from_size(size.into());
        if self.size != size || self.buffer.is_none() {
            self.buffer = Some(MemoryRenderBuffer::new(
                Fourcc::Argb8888,
                size,
                1,
                Transform::Normal,
                Some(vec![full]),
            ));
            self.size = size;
        }
        let mut context = self.buffer.as_mut().unwrap().render();
        let _ = context.draw(|data| {
            paint(&mut Canvas {
                data,
                width: size.0,
                height: size.1,
            });
            Ok::<_, ()>(vec![full])
        });
    }

    fn element(
        &self,
        renderer: &mut GlesRenderer,
        location: Point<i32, Logical>,
    ) -> Option<RoseRenderElement> {
        let buffer = self.buffer.as_ref()?;
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            location.to_f64().to_physical(1.0),
            buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .map_err(|err| tracing::warn!("failed to upload the bar: {err:?}"))
        .ok()
        .map(RoseRenderElement::from)
    }
}

pub struct Bar {
    font: Option<Font>,
    font_key: (String, f32),
    surface: Surface,
    drawn: Option<Content>,
    targets: Vec<(i32, i32, Target)>,
    menu_surface: Surface,
    /// The last line the status command printed.
    status: String,
    status_command: Option<String>,
    status_child: Option<Child>,
    /// Identifies the running status command, so lines from an old one are ignored.
    status_generation: u64,
    status_sender: Sender<(u64, String)>,
}

impl Bar {
    pub fn new(status_sender: Sender<(u64, String)>) -> Self {
        Self {
            font: None,
            font_key: (String::new(), 0.0),
            surface: Surface::default(),
            drawn: None,
            targets: Vec::new(),
            menu_surface: Surface::default(),
            status: String::new(),
            status_command: None,
            status_child: None,
            status_generation: 0,
            status_sender,
        }
    }

    /// Applies the config: loads the font and (re)starts the status command if they changed.
    pub fn configure(&mut self, config: &BarConfig) {
        let font_key = (config.font.clone(), config.font_size);
        if self.font_key != font_key {
            self.font = Font::load(&config.font, config.font_size.max(1.0))
                .map_err(|err| tracing::error!("can't load a font for the bar: {err}"))
                .ok();
            self.font_key = font_key;
            self.drawn = None;
        }
        if self.status_command != config.status_command {
            self.status_command = config.status_command.clone();
            self.start_status();
        }
    }

    fn start_status(&mut self) {
        self.stop_status();
        self.status.clear();
        self.status_generation += 1;
        let Some(command) = &self.status_command else {
            return;
        };
        let mut child = match Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                tracing::warn!("failed to run the status command `{command}`: {err}");
                return;
            }
        };
        let stdout = child.stdout.take().unwrap();
        let (sender, generation) = (self.status_sender.clone(), self.status_generation);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send((generation, line)).is_err() {
                    break;
                }
            }
        });
        self.status_child = Some(child);
    }

    fn stop_status(&mut self) {
        if let Some(mut child) = self.status_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn set_status(&mut self, generation: u64, line: String) {
        if generation == self.status_generation {
            self.status = line;
        }
    }

    /// The height of the bar, and of each menu row.
    pub fn row_height(&self, config: &BarConfig) -> i32 {
        let pad = (config.font_size * 0.3).round() as i32;
        match &self.font {
            Some(font) => font.line_height + 2 * pad,
            None => (config.font_size * 1.6).round() as i32,
        }
    }

    fn paint(&mut self, content: &Content) {
        self.targets.clear();
        let Some(font) = &mut self.font else {
            let theme = content.theme;
            self.surface.paint(content.size, |canvas| {
                canvas.fill((0, 0, canvas.width, canvas.height), theme.background)
            });
            return;
        };
        let targets = &mut self.targets;
        self.surface.paint(content.size, |canvas| {
            paint_bar(canvas, font, content, targets);
        });
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        self.stop_status();
    }
}

fn paint_bar(
    canvas: &mut Canvas,
    font: &mut Font,
    content: &Content,
    targets: &mut Vec<(i32, i32, Target)>,
) {
    let theme = &content.theme;
    let (width, height) = content.size;
    let pad = font.width(" ").max(4);
    let slant = height / 2;
    let text_y = (height - font.line_height) / 2;

    canvas.fill((0, 0, width, height), theme.background);

    // Sheets on the left, then the focused view's title.
    let mut x = pad / 2;
    let mut sheets = Vec::new();
    for (sheet, current) in &content.sheets {
        let label = sheet.to_string();
        let w = font.width(&label) + 2 * pad;
        sheets.push((x, w, label, *current));
        targets.push((x, x + w, Target::Sheet(*sheet)));
        x += w;
    }
    let sheets_end = x + pad / 2;
    let title = font.truncate(&content.title, width * 2 / 5);
    let title_end = if title.is_empty() {
        sheets_end
    } else {
        sheets_end + slant + pad + font.width(&title) + pad
    };

    // The mode on the right, with the status before it.
    let mode = font.truncate(&content.mode, width / 4);
    let mode_start = width - pad - font.width(&mode) - pad;
    let status_end = mode_start - slant - pad;
    let status = font.truncate(&content.status, (status_end - title_end - slant).max(0) / 2);
    let status_start = status_end - font.width(&status);

    // Each segment's slanted right edge overlaps the segment after it.
    let full = (0, height);
    let mode_color = content.mode_color.unwrap_or(theme.primary);
    let mode_text = if content.mode_color.is_some() {
        theme.background
    } else {
        theme.on_primary
    };
    canvas.fill_slanted((mode_start - slant, slant), (width, 0), full, mode_color);
    if !title.is_empty() {
        canvas.fill_slanted((0, 0), (title_end, slant), full, theme.accent);
    }
    canvas.fill_slanted((0, 0), (sheets_end, slant), full, theme.primary);

    for (x, w, label, current) in sheets {
        let color = if current {
            canvas.fill((x, 0, w, height), theme.secondary);
            theme.on_secondary
        } else {
            theme.on_primary
        };
        canvas.text(font, x + pad, text_y, &label, color);
    }
    if !title.is_empty() {
        canvas.text(
            font,
            sheets_end + slant + pad,
            text_y,
            &title,
            theme.on_accent,
        );
    }
    canvas.text(font, mode_start + pad, text_y, &mode, mode_text);
    canvas.text(font, status_start, text_y, &status, theme.text);

    // The other visible views, as tabs between the title and the status.
    let mut x = title_end + slant + pad;
    let tabs_end = status_start - 2 * pad;
    let tab_width = 12 * font.width("m");
    for (i, (label, window)) in content.tabs.iter().enumerate() {
        let label = font.truncate(label, tab_width);
        let w = font.width(&label);
        if x + w > tabs_end {
            break;
        }
        if i > 0 {
            canvas.fill(
                (x - pad, text_y + 2, 1, font.line_height - 4),
                theme.secondary,
            );
        }
        canvas.text(font, x, text_y, &label, theme.text);
        targets.push((x - pad, x + w + pad, Target::View(window.clone())));
        x += w + 2 * pad;
    }
}

impl Rose {
    /// The output's full rectangle.
    pub fn output_rect(&self) -> Rectangle<i32, Logical> {
        self.space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
            .unwrap_or_default()
    }

    /// The space the bar takes up at the top or bottom of the output.
    pub fn bar_rect(&self) -> Option<Rectangle<i32, Logical>> {
        if !self.config.bar.enabled {
            return None;
        }
        let output = self.output_rect();
        let h = self.bar.row_height(&self.config.bar).min(output.size.h);
        let y = match self.config.bar.position {
            BarPosition::Top => output.loc.y,
            BarPosition::Bottom => output.loc.y + output.size.h - h,
        };
        Some(Rectangle::new(
            (output.loc.x, y).into(),
            (output.size.w, h).into(),
        ))
    }

    /// The output minus the bar.
    pub fn work_area(&self) -> Rectangle<i32, Logical> {
        let mut area = self.output_rect();
        if let Some(bar) = self.bar_rect() {
            area.size.h -= bar.size.h;
            if self.config.bar.position == BarPosition::Top {
                area.loc.y += bar.size.h;
            }
        }
        area
    }

    /// The open menu's rectangle, next to the bar on the inside.
    pub fn menu_rect(&self) -> Option<Rectangle<i32, Logical>> {
        let menu = self.menu.as_ref()?;
        let area = self.work_area();
        let h = (self.bar.row_height(&self.config.bar) * menu.rows() as i32).min(area.size.h);
        let y = match self.config.bar.position {
            BarPosition::Top => area.loc.y,
            BarPosition::Bottom => area.loc.y + area.size.h - h,
        };
        Some(Rectangle::new(
            (area.loc.x, y).into(),
            (area.size.w, h).into(),
        ))
    }

    /// Whether the bar or menu covers `pos`, so views underneath don't get the pointer.
    pub fn over_panels(&self, pos: Point<f64, Logical>) -> bool {
        let pos = pos.to_i32_floor();
        self.bar_rect().is_some_and(|r| r.contains(pos))
            || self.menu_rect().is_some_and(|r| r.contains(pos))
    }

    fn bar_content(&self, size: (i32, i32)) -> Content {
        let mut sheets: Vec<(u8, bool)> = (0..wm::SHEETS)
            .filter(|s| {
                *s == self.current_sheet || self.views.iter().any(|v| v.placed && v.sheet == *s)
            })
            .map(|s| (s, s == self.current_sheet))
            .collect();
        // Sheet 0 goes last, like the 0 key.
        let sheet0 = sheets.first().is_some_and(|(s, _)| *s == 0);
        sheets.rotate_left(usize::from(sheet0));

        let title = self.focused.as_ref().map_or_else(String::new, |w| {
            let title = wm::title(w);
            if title.is_empty() {
                wm::app_id(w)
            } else {
                title
            }
        });
        let mut tabs: Vec<_> = self
            .views
            .iter()
            .filter(|v| self.is_visible(&v.window) && Some(&v.window) != self.focused.as_ref())
            .map(|v| (v.id, v.window.clone()))
            .collect();
        tabs.sort_by_key(|(id, _)| *id);
        let tabs = tabs
            .into_iter()
            .map(|(_, w)| {
                let title = wm::title(&w);
                (
                    if title.is_empty() {
                        wm::app_id(&w)
                    } else {
                        title
                    },
                    w,
                )
            })
            .collect();

        let colors = &self.config.ui.colorscheme;
        let mode_color = if self.mark_conflict() {
            Some(colors.conflict)
        } else if self.mode.is_insert() {
            Some(colors.insert)
        } else {
            None
        };

        Content {
            size,
            theme: self.config.bar.colors,
            sheets,
            title,
            tabs,
            mode: self.mode.describe(),
            mode_color,
            status: self.bar.status.clone(),
        }
    }

    /// Render elements for the menu and bar, front to back, repainting them if they changed.
    pub fn panel_elements(&mut self, renderer: &mut GlesRenderer) -> Vec<RoseRenderElement> {
        let mut elements = Vec::new();

        if let (Some(rect), Some(menu)) = (self.menu_rect(), &mut self.menu) {
            let size = (rect.size.w, rect.size.h);
            let row = self.bar.row_height(&self.config.bar);
            if menu.dirty || self.bar.menu_surface.size != size {
                let theme = self.config.bar.colors;
                if let Some(font) = &mut self.bar.font {
                    self.bar
                        .menu_surface
                        .paint(size, |canvas| menu.paint(canvas, font, &theme, row));
                }
            }
            elements.extend(self.bar.menu_surface.element(renderer, rect.loc));
        }

        if let Some(rect) = self.bar_rect() {
            let content = self.bar_content((rect.size.w, rect.size.h));
            if self.bar.drawn.as_ref() != Some(&content) {
                self.bar.paint(&content);
                self.bar.drawn = Some(content);
            }
            elements.extend(self.bar.surface.element(renderer, rect.loc));
        }

        elements
    }

    /// Handles a click on the bar or menu. Returns false if the click was elsewhere.
    pub fn panel_click(&mut self, pos: Point<f64, Logical>) -> bool {
        let pos = pos.to_i32_floor();
        if let Some(rect) = self.menu_rect() {
            if rect.contains(pos) {
                let row = (pos.y - rect.loc.y) / self.bar.row_height(&self.config.bar);
                let target = (row as usize)
                    .checked_sub(Menu::HEADER_ROWS)
                    .and_then(|r| self.menu.as_mut()?.accept_row(r));
                if let Some(target) = target {
                    self.close_menu();
                    self.run_target(target);
                }
                return true;
            }
        }
        let Some(rect) = self.bar_rect().filter(|r| r.contains(pos)) else {
            return false;
        };
        let x = pos.x - rect.loc.x;
        let target = self
            .bar
            .targets
            .iter()
            .find(|(start, end, _)| (*start..*end).contains(&x))
            .map(|(_, _, t)| t.clone());
        match target {
            Some(Target::Sheet(sheet)) => self.switch_to_sheet(sheet),
            Some(Target::View(window)) => {
                self.raise(&window);
                self.focus(Some(window));
            }
            None => {}
        }
        true
    }
}

impl Mode {
    /// The mode's name for the bar, with any input typed so far.
    pub fn describe(&self) -> String {
        match self {
            Mode::Normal => "normal".into(),
            Mode::GroupAssign { input } => format!("group: {input}_"),
            Mode::MarkAssign { input } => format!("mark: {}", input.unwrap_or('_')),
            Mode::MarkSelect { switch: false } => "mark-select".into(),
            Mode::MarkSelect { switch: true } => "mark-switch-select".into(),
            Mode::SheetAssign => "sheet-assign".into(),
            Mode::InputGrab { .. } => "input-grab".into(),
            Mode::Move => "move".into(),
            Mode::Resize => "resize".into(),
            Mode::Menu => "menu".into(),
        }
    }
}

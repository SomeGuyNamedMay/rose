//! Window management modeled on hikari: views live on sheets, can be grouped and marked, and
//! sheets can be tiled with layouts.

use smithay::{
    backend::renderer::element::solid::SolidColorBuffer,
    desktop::Window,
    input::keyboard::Keysym,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    utils::{Logical, Point, Rectangle, Size, SERIAL_COUNTER},
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use crate::{
    action::{Action, Align, Cycle, Direction, Exchange, ModeKind, Position, SheetTarget},
    config::{Color, Config, KeyBinding},
    grab::Edges,
    layout::Layout,
    Rose,
};

/// Sheets 1-9 are general purpose; views on sheet 0 are always visible.
pub const SHEETS: u8 = 10;

pub enum Mode {
    Normal,
    /// Typing a group name for the focused view.
    GroupAssign {
        input: String,
    },
    /// Typing a mark for the focused view.
    MarkAssign {
        input: Option<char>,
    },
    /// Waiting for a mark to show, or to switch to its sheet.
    MarkSelect {
        switch: bool,
    },
    /// Waiting for a layout register.
    Layout,
    /// Waiting for a sheet number to move the focused view to.
    SheetAssign,
    /// All keys go to the focused view until `exit` is pressed again.
    InputGrab {
        exit: KeyBinding,
    },
    /// Moving or resizing the focused view with the pointer until a key is released.
    Move,
    Resize,
}

impl Mode {
    /// Whether the mode is waiting for input, shown with the insert border color.
    pub fn is_insert(&self) -> bool {
        !matches!(self, Self::Normal | Self::Move | Self::Resize)
    }
}

pub struct SheetLayout {
    pub layout: Layout,
    /// Tiled views in layout order.
    pub order: Vec<Window>,
}

#[derive(Clone, Copy, Default)]
pub struct Maximized {
    pub horizontal: bool,
    pub vertical: bool,
}

impl Maximized {
    fn any(self) -> bool {
        self.horizontal || self.vertical
    }
}

/// Keeps the opposite edge in place while a view is resized from its top or left.
#[derive(Clone, Copy)]
pub struct ResizeAnchor {
    pub edges: Edges,
    pub initial: Rectangle<i32, Logical>,
    /// The grab ended; the anchor is dropped after the next commit.
    pub finished: bool,
}

pub struct View {
    /// Creation order, used for cycling.
    pub id: u64,
    pub window: Window,
    pub sheet: u8,
    pub group: String,
    /// Floating views are never tiled.
    pub floating: bool,
    /// Invisible views aren't shown when switching to their sheet, and are never tiled.
    pub invisible: bool,
    pub maximized: Maximized,
    /// The content rectangle this view's layout assigned it, if it is tiled.
    pub tile: Option<Rectangle<i32, Logical>>,
    /// Geometry from before the view was tiled or maximized, restored when it no longer is.
    pub natural: Option<Rectangle<i32, Logical>>,
    /// Geometry the view was first placed with, restored by `view-reset-geometry`.
    pub original: Rectangle<i32, Logical>,
    /// Location in the space, kept while the view is hidden.
    pub location: Point<i32, Logical>,
    /// Whether the view has been placed after its first buffer.
    pub placed: bool,
    pub resize: Option<ResizeAnchor>,
    /// Top, bottom, left and right borders.
    pub borders: [SolidColorBuffer; 4],
}

impl View {
    fn tileable(&self, sheet: u8) -> bool {
        self.placed && self.sheet == sheet && !self.floating && !self.invisible
    }
}

/// Runs a shell command without waiting for it.
pub fn spawn(command: &str) {
    match std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .spawn()
    {
        Ok(mut child) => {
            // Reap the child when it exits.
            std::thread::spawn(move || child.wait());
        }
        Err(err) => tracing::warn!("failed to run `{command}`: {err}"),
    }
}

fn shrink(rect: Rectangle<i32, Logical>, by: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        rect.loc + Point::from((by, by)),
        Size::from(((rect.size.w - 2 * by).max(1), (rect.size.h - 2 * by).max(1))),
    )
}

fn right(rect: &Rectangle<i32, Logical>) -> i32 {
    rect.loc.x + rect.size.w
}

fn bottom(rect: &Rectangle<i32, Logical>) -> i32 {
    rect.loc.y + rect.size.h
}

impl Rose {
    pub fn view(&self, window: &Window) -> Option<&View> {
        self.views.iter().find(|v| &v.window == window)
    }

    pub fn view_mut(&mut self, window: &Window) -> Option<&mut View> {
        self.views.iter_mut().find(|v| &v.window == window)
    }

    pub fn is_visible(&self, window: &Window) -> bool {
        self.space.element_location(window).is_some()
    }

    /// Visible views, bottom to top.
    fn visible(&self) -> Vec<Window> {
        self.views
            .iter()
            .filter(|v| self.is_visible(&v.window))
            .map(|v| v.window.clone())
            .collect()
    }

    fn by_id(&self, mut windows: Vec<Window>) -> Vec<Window> {
        windows.sort_by_key(|w| self.view(w).map(|v| v.id));
        windows
    }

    fn focused_group(&self) -> Option<String> {
        self.focused
            .as_ref()
            .and_then(|w| self.view(w))
            .map(|v| v.group.clone())
    }

    fn in_group(&self, window: &Window, group: &str) -> bool {
        self.view(window).is_some_and(|v| v.group == group)
    }

    /// The window's geometry in the space, or where it would be if it is hidden.
    pub fn geometry(&self, window: &Window) -> Rectangle<i32, Logical> {
        let location = self
            .space
            .element_location(window)
            .or_else(|| self.view(window).map(|v| v.location))
            .unwrap_or_default();
        Rectangle::new(location, window.geometry().size)
    }

    fn output_area(&self) -> Rectangle<i32, Logical> {
        self.space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
            .unwrap_or_default()
    }

    /// The output minus the gap around its edge.
    fn usable_area(&self) -> Rectangle<i32, Logical> {
        shrink(self.output_area(), self.config.ui.gap)
    }

    /// Where view content can go so that borders stay inside the usable area.
    fn content_area(&self) -> Rectangle<i32, Logical> {
        shrink(self.usable_area(), self.config.ui.border)
    }

    // Stacking and visibility

    /// Syncs the space's stacking order with `views`, keeping sheet 0 below other sheets.
    pub fn restack(&mut self) {
        let visible = self.visible();
        let (sheet0, rest): (Vec<_>, Vec<_>) = visible
            .into_iter()
            .partition(|w| self.view(w).is_some_and(|v| v.sheet == 0));
        for window in sheet0.iter().chain(&rest) {
            self.space.raise_element(window, false);
        }
    }

    /// Maps a view at its last location. Call `update_workspace` afterwards.
    fn show(&mut self, window: &Window) {
        if let Some(view) = self.view(window).filter(|v| v.placed) {
            if !self.is_visible(window) {
                let location = view.location;
                self.space.map_element(window.clone(), location, false);
            }
        }
    }

    /// Unmaps a view, remembering its location. Call `update_workspace` afterwards.
    fn hide(&mut self, window: &Window) {
        if let Some(location) = self.space.element_location(window) {
            self.space.unmap_elem(window);
            if let Some(view) = self.view_mut(window) {
                view.location = location;
            }
        }
    }

    /// Restacks after visibility changes and moves focus off hidden views.
    fn update_workspace(&mut self) {
        self.restack();
        if !self.focused.as_ref().is_some_and(|w| self.is_visible(w)) {
            self.focus_top();
        }
    }

    pub fn raise(&mut self, window: &Window) {
        if let Some(index) = self.views.iter().position(|v| &v.window == window) {
            let view = self.views.remove(index);
            self.views.push(view);
            self.restack();
        }
    }

    pub fn lower(&mut self, window: &Window) {
        if let Some(index) = self.views.iter().position(|v| &v.window == window) {
            let view = self.views.remove(index);
            self.views.insert(0, view);
            self.restack();
        }
    }

    pub fn set_location(&mut self, window: &Window, location: Point<i32, Logical>) {
        let Some(view) = self.view_mut(window) else {
            return;
        };
        view.location = location;
        if self.is_visible(window) {
            self.space.map_element(window.clone(), location, false);
            self.restack();
        }
    }

    // Focus

    pub fn focus(&mut self, window: Option<Window>) {
        // An input grab keeps focus on its view.
        if matches!(self.mode, Mode::InputGrab { .. }) && window != self.focused {
            return;
        }
        if let Some(window) = &window {
            self.raise(window);
        }
        self.focused = window.clone();

        let surface = window
            .as_ref()
            .map(|w| w.toplevel().unwrap().wl_surface().clone());
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());

        for view in &self.views {
            view.window
                .set_activated(Some(&view.window) == window.as_ref());
            let toplevel = view.window.toplevel().unwrap();
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        }
    }

    fn focus_top(&mut self) {
        let top = self.visible().pop();
        self.focus(top);
    }

    // View lifecycle

    pub fn add_view(&mut self, window: Window) {
        let id = self.next_view_id;
        self.next_view_id += 1;
        self.views.push(View {
            id,
            window,
            sheet: self.current_sheet,
            group: String::new(),
            floating: false,
            invisible: false,
            maximized: Maximized::default(),
            tile: None,
            natural: None,
            original: Rectangle::default(),
            location: Point::default(),
            placed: false,
            resize: None,
            borders: Default::default(),
        });
    }

    /// Places a view once its first buffer has arrived, applying any view rule for its app id.
    pub fn place_view(&mut self, window: &Window) {
        let app_id = with_states(window.toplevel().unwrap().wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().unwrap().app_id.clone())
        })
        .unwrap_or_default();
        let rule = self.config.views.get(&app_id).cloned().unwrap_or_default();

        let size = window.geometry().size;
        let location = match rule.position {
            Some(position) => Point::from((position.x, position.y)),
            None => {
                // Centered on the pointer, but kept on the output.
                let pointer = self
                    .seat
                    .get_pointer()
                    .unwrap()
                    .current_location()
                    .to_i32_round();
                let area = self.content_area();
                let clamp = |pos: i32, len: i32, start: i32, avail: i32| {
                    (pos - len / 2).clamp(start, (start + avail - len).max(start))
                };
                Point::from((
                    clamp(pointer.x, size.w, area.loc.x, area.size.w),
                    clamp(pointer.y, size.h, area.loc.y, area.size.h),
                ))
            }
        };

        let current = self.current_sheet;
        let Some(view) = self.view_mut(window) else {
            return;
        };
        view.sheet = rule.sheet.filter(|s| *s < SHEETS).unwrap_or(current);
        view.group = rule.group.unwrap_or_else(|| {
            if app_id.is_empty() {
                "default".into()
            } else {
                app_id
            }
        });
        view.floating = rule.floating;
        view.invisible = rule.invisible;
        view.location = location;
        view.original = Rectangle::new(location, size);
        view.placed = true;
        let sheet = view.sheet;

        if let Some(mark) = rule.mark.filter(char::is_ascii_lowercase) {
            self.marks.entry(mark).or_insert_with(|| window.clone());
        }
        if sheet == current || sheet == 0 {
            self.show(window);
            self.restack();
            self.focus(Some(window.clone()));
        }
    }

    pub fn remove_view(&mut self, window: &Window) {
        // Modes act on the focused view, so end them when it goes away.
        if self.focused.as_ref() == Some(window) && !matches!(self.mode, Mode::Normal) {
            let pointer = self.seat.get_pointer().unwrap();
            if pointer.is_grabbed() {
                pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), 0);
            }
            self.mode = Mode::Normal;
        }

        let Some(index) = self.views.iter().position(|v| &v.window == window) else {
            return;
        };
        self.views.remove(index);
        self.space.unmap_elem(window);
        self.marks.retain(|_, w| w != window);
        for sheet in 0..SHEETS {
            if let Some(layout) = &mut self.sheet_layouts[sheet as usize] {
                if let Some(i) = layout.order.iter().position(|w| w == window) {
                    layout.order.remove(i);
                    self.arrange_sheet(sheet);
                }
            }
        }

        if self.focused.as_ref() == Some(window) {
            self.focused = None;
            self.focus_top();
        }
        if self.quit_requested && self.views.is_empty() {
            self.loop_signal.stop();
        }
    }

    // Geometry

    /// Moves and resizes a view, asking the client for the new size.
    fn configure(&mut self, window: &Window, rect: Rectangle<i32, Logical>) {
        let Some(view) = self.view(window) else {
            return;
        };
        let tiled = view.tile.is_some();
        let maximized = view.maximized.horizontal && view.maximized.vertical;
        self.set_location(window, rect.loc);

        let toplevel = window.toplevel().unwrap();
        toplevel.with_pending_state(|state| {
            state.size = Some(rect.size);
            let mut set = |s: xdg_toplevel::State, on: bool| {
                if on {
                    state.states.set(s);
                } else {
                    state.states.unset(s);
                }
            };
            set(xdg_toplevel::State::Maximized, maximized);
            set(xdg_toplevel::State::TiledLeft, tiled);
            set(xdg_toplevel::State::TiledRight, tiled);
            set(xdg_toplevel::State::TiledTop, tiled);
            set(xdg_toplevel::State::TiledBottom, tiled);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn remember_natural(&mut self, window: &Window) {
        let geometry = self.geometry(window);
        if let Some(view) = self.view_mut(window) {
            view.natural.get_or_insert(geometry);
        }
    }

    /// Applies a view's tile and maximization, or restores its natural geometry if it has neither.
    fn apply_geometry(&mut self, window: &Window) {
        let area = self.content_area();
        let Some(view) = self.view_mut(window) else {
            return;
        };
        let maximized = view.maximized;
        if view.tile.is_none() && !maximized.any() {
            if let Some(natural) = view.natural.take() {
                self.configure(window, natural);
            }
            return;
        }

        let Some(mut rect) = view.tile.or(view.natural) else {
            return;
        };
        if maximized.horizontal {
            rect.loc.x = area.loc.x;
            rect.size.w = area.size.w;
        }
        if maximized.vertical {
            rect.loc.y = area.loc.y;
            rect.size.h = area.size.h;
        }
        self.configure(window, rect);
    }

    fn view_move(&mut self, window: &Window, direction: Direction) {
        let step = self.config.ui.step;
        let mut location = self.geometry(window).loc;
        match direction {
            Direction::Up => location.y -= step,
            Direction::Down => location.y += step,
            Direction::Left => location.x -= step,
            Direction::Right => location.x += step,
        }
        self.set_location(window, location);
    }

    fn view_resize(&mut self, window: &Window, direction: Direction, grow: bool) {
        let step = if grow {
            self.config.ui.step
        } else {
            -self.config.ui.step
        };
        let mut rect = self.geometry(window);
        match direction {
            Direction::Up => {
                rect.loc.y -= step;
                rect.size.h += step;
            }
            Direction::Down => rect.size.h += step,
            Direction::Left => {
                rect.loc.x -= step;
                rect.size.w += step;
            }
            Direction::Right => rect.size.w += step,
        }
        rect.size.w = rect.size.w.max(1);
        rect.size.h = rect.size.h.max(1);
        self.configure(window, rect);
    }

    fn view_move_to(&mut self, window: &Window, position: Position) {
        let area = self.content_area();
        let size = window.geometry().size;
        let align = |align: Align, start: i32, avail: i32, len: i32| match align {
            Align::Start => start,
            Align::Middle => start + (avail - len) / 2,
            Align::End => start + avail - len,
        };
        let location = Point::from((
            align(position.horizontal, area.loc.x, area.size.w, size.w),
            align(position.vertical, area.loc.y, area.size.h, size.h),
        ));
        self.set_location(window, location);
    }

    /// Moves a view until its border meets the output edge or another view, keeping the gap.
    fn view_snap(&mut self, window: &Window, direction: Direction) {
        let border = self.config.ui.border;
        let gap = self.config.ui.gap;
        let area = self.usable_area();
        let geometry = self.geometry(window);
        let outer = shrink(geometry, -border);
        let others: Vec<_> = self
            .visible()
            .iter()
            .filter(|w| *w != window)
            .map(|w| shrink(self.geometry(w), -border))
            .collect();
        let overlaps_x =
            |o: &&Rectangle<i32, Logical>| o.loc.x < right(&outer) && outer.loc.x < right(o);
        let overlaps_y =
            |o: &&Rectangle<i32, Logical>| o.loc.y < bottom(&outer) && outer.loc.y < bottom(o);

        let mut location = geometry.loc;
        match direction {
            Direction::Up => {
                let edge = others
                    .iter()
                    .filter(overlaps_x)
                    .filter(|o| bottom(o) <= outer.loc.y)
                    .map(|o| bottom(o) + gap)
                    .fold(area.loc.y, i32::max);
                location.y = edge + border;
            }
            Direction::Down => {
                let edge = others
                    .iter()
                    .filter(overlaps_x)
                    .filter(|o| o.loc.y >= bottom(&outer))
                    .map(|o| o.loc.y - gap)
                    .fold(bottom(&area), i32::min);
                location.y = edge - border - geometry.size.h;
            }
            Direction::Left => {
                let edge = others
                    .iter()
                    .filter(overlaps_y)
                    .filter(|o| right(o) <= outer.loc.x)
                    .map(|o| right(o) + gap)
                    .fold(area.loc.x, i32::max);
                location.x = edge + border;
            }
            Direction::Right => {
                let edge = others
                    .iter()
                    .filter(overlaps_y)
                    .filter(|o| o.loc.x >= right(&outer))
                    .map(|o| o.loc.x - gap)
                    .fold(right(&area), i32::min);
                location.x = edge - border - geometry.size.w;
            }
        }
        self.set_location(window, location);
    }

    pub fn toggle_maximize(&mut self, window: &Window, horizontal: bool, vertical: bool) {
        self.remember_natural(window);
        let Some(view) = self.view_mut(window) else {
            return;
        };
        let m = &mut view.maximized;
        if horizontal && vertical {
            let full = m.horizontal && m.vertical;
            m.horizontal = !full;
            m.vertical = !full;
        } else {
            m.horizontal ^= horizontal;
            m.vertical ^= vertical;
        }
        self.apply_geometry(window);
    }

    pub fn set_maximized(&mut self, window: &Window, maximized: bool) {
        self.remember_natural(window);
        if let Some(view) = self.view_mut(window) {
            view.maximized = Maximized {
                horizontal: maximized,
                vertical: maximized,
            };
        }
        self.apply_geometry(window);
    }

    /// Forgets maximization without changing geometry, for when the user takes over sizing.
    pub fn clear_maximized(&mut self, window: &Window) {
        if let Some(view) = self.view_mut(window) {
            view.maximized = Maximized::default();
        }
        window.toplevel().unwrap().with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Maximized);
        });
    }

    // Layouts

    fn arrange_sheet(&mut self, sheet: u8) {
        let Some(layout) = &self.sheet_layouts[sheet as usize] else {
            return;
        };
        let gap = self.config.ui.gap;
        let border = self.config.ui.border;

        // Each tile is followed by a gap, so leave one before the first tile too.
        let output = self.output_area();
        let area = Rectangle::new(
            output.loc + Point::from((gap, gap)),
            Size::from((output.size.w - gap, output.size.h - gap)),
        );
        let sizes: Vec<_> = layout.order.iter().map(|w| w.geometry().size).collect();
        let tiles = layout.layout.arrange(area, &sizes);
        let order = layout.order.clone();

        // Views the layout has no room for drop out of it.
        for window in &order[tiles.len()..] {
            if let Some(view) = self.view_mut(window) {
                view.tile = None;
            }
            self.apply_geometry(window);
        }
        if let Some(layout) = &mut self.sheet_layouts[sheet as usize] {
            layout.order.truncate(tiles.len());
        }

        for (window, tile) in order.iter().zip(tiles) {
            let content = Rectangle::new(
                tile.loc + Point::from((border, border)),
                Size::from((
                    (tile.size.w - gap - 2 * border).max(1),
                    (tile.size.h - gap - 2 * border).max(1),
                )),
            );
            self.remember_natural(window);
            if let Some(view) = self.view_mut(window) {
                view.tile = Some(content);
            }
            self.apply_geometry(window);
        }
    }

    pub fn arrange_all(&mut self) {
        for sheet in 0..SHEETS {
            self.arrange_sheet(sheet);
        }
    }

    /// Takes a view out of whichever layout it is in and restores its natural geometry.
    fn layout_remove(&mut self, window: &Window) {
        for sheet in 0..SHEETS {
            if let Some(layout) = &mut self.sheet_layouts[sheet as usize] {
                if let Some(i) = layout.order.iter().position(|w| w == window) {
                    layout.order.remove(i);
                    if let Some(view) = self.view_mut(window) {
                        view.tile = None;
                    }
                    self.apply_geometry(window);
                    self.arrange_sheet(sheet);
                }
            }
        }
    }

    /// Tiles every eligible view of the current sheet, topmost first.
    fn layout_apply(&mut self, register: char) {
        let Some(layout) = self.config.layouts.get(&register).cloned() else {
            return;
        };
        let sheet = self.current_sheet;
        if let Some(old) = self.sheet_layouts[sheet as usize].take() {
            for window in &old.order {
                if let Some(view) = self.view_mut(window) {
                    view.tile = None;
                }
            }
        }

        let order: Vec<Window> = self
            .views
            .iter()
            .rev()
            .filter(|v| v.tileable(sheet))
            .map(|v| v.window.clone())
            .collect();
        for window in &order {
            self.show(window);
        }
        self.sheet_layouts[sheet as usize] = Some(SheetLayout { layout, order });
        self.arrange_sheet(sheet);
        self.update_workspace();
    }

    fn layout_restack(&mut self, append: bool) {
        let sheet = self.current_sheet;
        if self.sheet_layouts[sheet as usize].is_none() {
            self.layout_apply(char::from(b'0' + sheet));
            return;
        }

        let order = &self.sheet_layouts[sheet as usize].as_ref().unwrap().order;
        let new: Vec<Window> = self
            .views
            .iter()
            .rev()
            .filter(|v| v.tileable(sheet) && !order.contains(&v.window))
            .map(|v| v.window.clone())
            .collect();
        for window in &new {
            self.show(window);
        }

        let layout = self.sheet_layouts[sheet as usize].as_mut().unwrap();
        if append {
            layout.order.extend(new);
        } else {
            layout.order.splice(0..0, new);
        }
        self.arrange_sheet(sheet);
        self.update_workspace();
    }

    fn layout_reset(&mut self) {
        let sheet = self.current_sheet;
        let Some(layout) = &self.sheet_layouts[sheet as usize] else {
            return;
        };
        for window in layout.order.clone() {
            if let Some(view) = self.view_mut(&window) {
                view.maximized = Maximized::default();
            }
        }
        self.arrange_sheet(sheet);
    }

    fn layout_exchange(&mut self, exchange: Exchange) {
        let sheet = self.current_sheet;
        let Some(focused) = self.focused.clone() else {
            return;
        };
        let Some(layout) = &mut self.sheet_layouts[sheet as usize] else {
            return;
        };
        let Some(i) = layout.order.iter().position(|w| *w == focused) else {
            return;
        };
        let n = layout.order.len();
        let j = match exchange {
            Exchange::Next => (i + 1) % n,
            Exchange::Prev => (i + n - 1) % n,
            Exchange::Main => 0,
        };
        layout.order.swap(i, j);
        self.arrange_sheet(sheet);
    }

    fn layout_order(&self) -> Vec<Window> {
        self.sheet_layouts[self.current_sheet as usize]
            .as_ref()
            .map(|l| {
                l.order
                    .iter()
                    .filter(|w| self.is_visible(w))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    // Sheets

    fn resolve_sheet(&self, target: SheetTarget) -> u8 {
        match target {
            SheetTarget::Number(n) => n,
            SheetTarget::Alternate => self.alternate_sheet,
            SheetTarget::Current => self.current_sheet,
        }
    }

    /// Shows only the views matching `show`.
    fn set_workspace(&mut self, show: impl Fn(&View) -> bool) {
        let (shown, hidden): (Vec<&View>, Vec<&View>) = self
            .views
            .iter()
            .filter(|v| v.placed)
            .partition(|v| show(v));
        let shown: Vec<Window> = shown.iter().map(|v| v.window.clone()).collect();
        let hidden: Vec<Window> = hidden.iter().map(|v| v.window.clone()).collect();
        for window in &hidden {
            self.hide(window);
        }
        for window in &shown {
            self.show(window);
        }
        self.update_workspace();
    }

    /// Switches to a sheet. Switching to the current sheet resets it: borrowed views are hidden,
    /// hidden views are shown and invisible views are hidden.
    pub fn switch_to_sheet(&mut self, sheet: u8) {
        if sheet != self.current_sheet {
            self.alternate_sheet = self.current_sheet;
            self.current_sheet = sheet;
        }
        self.set_workspace(|v| (v.sheet == 0 || v.sheet == sheet) && !v.invisible);
    }

    fn switch_inhabited(&mut self, next: bool) {
        let inhabited = |s: u8| self.views.iter().any(|v| v.placed && v.sheet == s);
        let current = match self.current_sheet {
            0 if !next => 10,
            s => s as i32,
        };
        let found = (1..=9)
            .map(|k| {
                if next {
                    current - 1 + k
                } else {
                    current - 1 - k
                }
            })
            .map(|s| (s.rem_euclid(9) + 1) as u8)
            .find(|s| inhabited(*s));
        if let Some(sheet) = found {
            self.switch_to_sheet(sheet);
        }
    }

    fn pin_to_sheet(&mut self, window: &Window, target: SheetTarget) {
        let sheet = self.resolve_sheet(target);
        if self.view(window).is_none_or(|v| v.sheet == sheet) {
            return;
        }
        self.layout_remove(window);
        if let Some(view) = self.view_mut(window) {
            view.sheet = sheet;
        }
        if sheet != self.current_sheet && sheet != 0 {
            self.hide(window);
        }
        self.update_workspace();
    }

    // Cycling

    fn cycle_through(&mut self, candidates: Vec<Window>, cycle: Cycle) {
        let n = candidates.len();
        if n == 0 {
            return;
        }
        let current = self
            .focused
            .as_ref()
            .and_then(|f| candidates.iter().position(|w| w == f));
        let index = match (cycle, current) {
            (Cycle::First, _) => 0,
            (Cycle::Last, _) => n - 1,
            (Cycle::Next, Some(i)) => (i + 1) % n,
            (Cycle::Prev, Some(i)) => (i + n - 1) % n,
            (Cycle::Next, None) => 0,
            (Cycle::Prev, None) => n - 1,
        };
        self.focus(Some(candidates[index].clone()));
    }

    /// Focuses the topmost view of the next or previous group, ordered by name.
    fn group_cycle(&mut self, cycle: Cycle) {
        let visible = self.visible();
        let mut groups: Vec<String> = visible
            .iter()
            .filter_map(|w| self.view(w))
            .map(|v| v.group.clone())
            .collect();
        groups.sort();
        groups.dedup();
        let n = groups.len();
        if n == 0 {
            return;
        }

        let current = self
            .focused_group()
            .and_then(|g| groups.iter().position(|x| *x == g));
        let index = match (cycle, current) {
            (Cycle::First, _) => 0,
            (Cycle::Last, _) => n - 1,
            (Cycle::Next, Some(i)) => (i + 1) % n,
            (Cycle::Prev, Some(i)) => (i + n - 1) % n,
            (Cycle::Next, None) => 0,
            (Cycle::Prev, None) => n - 1,
        };
        let top = visible
            .into_iter()
            .rev()
            .find(|w| self.in_group(w, &groups[index]));
        self.focus(top);
    }

    fn group_views(&self, group: &str) -> Vec<Window> {
        self.visible()
            .into_iter()
            .filter(|w| self.in_group(w, group))
            .collect()
    }

    /// Moves a group's views to the top or bottom of the stack, keeping their order.
    fn restack_group(&mut self, group: &str, raise: bool) {
        let (members, others): (Vec<View>, Vec<View>) =
            self.views.drain(..).partition(|v| v.group == group);
        self.views = if raise {
            others.into_iter().chain(members).collect()
        } else {
            members.into_iter().chain(others).collect()
        };
        self.restack();
    }

    // Marks

    fn mark_show(&mut self, mark: char, switch: bool) {
        let Some(window) = self.marks.get(&mark).cloned() else {
            if let Some(command) = self.config.marks.get(&mark) {
                spawn(command);
            }
            return;
        };
        if switch {
            let sheet = self.view(&window).map_or(0, |v| v.sheet);
            if sheet != 0 && sheet != self.current_sheet {
                self.switch_to_sheet(sheet);
            }
        }
        // Showing a view from another sheet borrows it into the workspace.
        self.show(&window);
        self.update_workspace();
        self.focus(Some(window));
    }

    /// Whether the mark being typed already belongs to another view.
    pub fn mark_conflict(&self) -> bool {
        match self.mode {
            Mode::MarkAssign { input: Some(mark) } => self
                .marks
                .get(&mark)
                .is_some_and(|w| Some(w) != self.focused.as_ref()),
            _ => false,
        }
    }

    // Modes

    fn enter_mode(&mut self, kind: ModeKind, trigger: Option<KeyBinding>) {
        if self.focused.is_none()
            && !matches!(
                kind,
                ModeKind::Layout | ModeKind::MarkSelect | ModeKind::MarkSwitchSelect
            )
        {
            return;
        }
        self.mode = match kind {
            ModeKind::GroupAssign => Mode::GroupAssign {
                input: String::new(),
            },
            ModeKind::MarkAssign => Mode::MarkAssign { input: None },
            ModeKind::MarkSelect => Mode::MarkSelect { switch: false },
            ModeKind::MarkSwitchSelect => Mode::MarkSelect { switch: true },
            ModeKind::Layout => Mode::Layout,
            ModeKind::SheetAssign => Mode::SheetAssign,
            ModeKind::InputGrab => match trigger {
                Some(exit) => Mode::InputGrab { exit },
                None => return,
            },
            ModeKind::Move | ModeKind::Resize => {
                self.start_keyboard_grab(kind == ModeKind::Resize);
                return;
            }
        };
    }

    /// Handles a key pressed in a mode that waits for input.
    pub fn handle_mode_key(&mut self, sym: Keysym) {
        let ch = sym.key_char().filter(|c| !c.is_control());
        let enter = sym == Keysym::Return || sym == Keysym::KP_Enter;
        let focused = self.focused.clone();

        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::GroupAssign { mut input } => {
                if sym == Keysym::Escape {
                } else if enter && !input.is_empty() {
                    if let Some(view) = focused.and_then(|w| self.view_mut(&w)) {
                        view.group = input;
                    }
                } else {
                    if sym == Keysym::BackSpace {
                        input.pop();
                    } else if let Some(c) = ch {
                        input.push(c);
                    }
                    self.mode = Mode::GroupAssign { input };
                }
            }
            Mode::MarkAssign { input } => {
                let Some(window) = focused else {
                    return;
                };
                if sym == Keysym::Escape {
                } else if enter {
                    self.mode = Mode::MarkAssign { input };
                    if self.mark_conflict() {
                        return;
                    }
                    self.mode = Mode::Normal;
                    self.marks.retain(|_, w| *w != window);
                    if let Some(mark) = input {
                        self.marks.insert(mark, window);
                    }
                } else if sym == Keysym::BackSpace {
                    self.mode = Mode::MarkAssign { input: None };
                } else {
                    let input = ch.filter(char::is_ascii_lowercase).or(input);
                    self.mode = Mode::MarkAssign { input };
                }
            }
            Mode::MarkSelect { switch } => {
                if let Some(mark) = ch.filter(char::is_ascii_lowercase) {
                    self.mark_show(mark, switch);
                }
            }
            Mode::Layout => {
                if let Some(register) = ch.filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                {
                    self.layout_apply(register);
                }
            }
            Mode::SheetAssign => {
                if let (Some(window), Some(sheet)) = (focused, ch.and_then(|c| c.to_digit(10))) {
                    self.pin_to_sheet(&window, SheetTarget::Number(sheet as u8));
                }
            }
            other => self.mode = other,
        }
    }

    pub fn border_color(&self, window: &Window) -> Color {
        let colors = &self.config.ui.colorscheme;
        if self.focused.as_ref() == Some(window) {
            if self.mark_conflict() {
                colors.conflict
            } else if self.mode.is_insert() {
                colors.insert
            } else {
                colors.active
            }
        } else if self
            .focused_group()
            .is_some_and(|g| self.in_group(window, &g))
        {
            colors.grouped
        } else {
            colors.inactive
        }
    }

    // Actions

    fn quit(&mut self) {
        if self.quit_requested || self.views.is_empty() {
            self.loop_signal.stop();
            return;
        }
        self.quit_requested = true;
        for view in &self.views {
            view.window.toplevel().unwrap().send_close();
        }
    }

    pub fn reload(&mut self) {
        let config = match Config::load(&self.config_path) {
            Ok(config) => config,
            Err(err) => {
                tracing::error!("failed to reload config: {err}");
                return;
            }
        };
        let keyboard_changed = config.keyboard != self.config.keyboard;
        self.config = config;

        if keyboard_changed {
            let settings = self.config.keyboard.clone();
            let keyboard = self.seat.get_keyboard().unwrap();
            if let Err(err) = keyboard.set_xkb_config(self, settings.xkb_config()) {
                tracing::error!("invalid xkb config: {err:?}");
            }
            keyboard.change_repeat_info(settings.repeat_rate, settings.repeat_delay);
        }
        self.arrange_all();
        tracing::info!("reloaded {}", self.config_path.display());
    }

    /// Runs an action. `trigger` is the key binding that ran it, if any.
    pub fn run_action(&mut self, action: Action, trigger: Option<KeyBinding>) {
        let focused = self.focused.clone();
        let group = self.focused_group();
        let current = self.current_sheet;

        match action {
            Action::Quit => self.quit(),
            Action::Reload => self.reload(),
            Action::Command(name) => {
                if let Some(command) = self.config.actions.get(&name) {
                    spawn(command);
                }
            }

            Action::GroupCycle(cycle) => self.group_cycle(cycle),
            Action::GroupCycleView(cycle) => {
                if let Some(group) = group {
                    let views = self.by_id(self.group_views(&group));
                    self.cycle_through(views, cycle);
                }
            }
            Action::GroupHide => {
                if let Some(group) = group {
                    for window in self.group_views(&group) {
                        self.hide(&window);
                    }
                    self.update_workspace();
                }
            }
            Action::GroupLower => {
                if let Some(group) = group {
                    self.restack_group(&group, false);
                }
            }
            Action::GroupRaise => {
                if let Some(group) = group {
                    self.restack_group(&group, true);
                }
            }
            Action::GroupOnly => {
                if let Some(group) = group {
                    let keep = self.group_views(&group);
                    self.set_workspace(|v| keep.contains(&v.window));
                }
            }

            Action::LayoutApply(register) => self.layout_apply(register),
            Action::LayoutCycleView(cycle) => {
                let order = self.layout_order();
                self.cycle_through(order, cycle);
            }
            Action::LayoutExchangeView(exchange) => self.layout_exchange(exchange),
            Action::LayoutReset => self.layout_reset(),
            Action::LayoutRestack { append } => self.layout_restack(append),

            Action::MarkShow(mark) => self.mark_show(mark, false),
            Action::MarkSwitchTo(mark) => self.mark_show(mark, true),

            Action::EnterMode(kind) => self.enter_mode(kind, trigger),

            Action::SheetShowAll => self.set_workspace(|v| v.sheet == current),
            Action::SheetShowGroup => {
                if let Some(group) = group {
                    self.set_workspace(|v| v.sheet == current && v.group == group);
                }
            }
            Action::SheetShowInvisible => self.set_workspace(|v| v.sheet == current && v.invisible),

            Action::WorkspaceClear => self.set_workspace(|_| false),
            Action::WorkspaceShowAll => self.set_workspace(|_| true),
            Action::WorkspaceShowGroup => {
                if let (Some(window), Some(group)) = (focused, group) {
                    self.raise(&window);
                    self.set_workspace(|v| v.group == group);
                }
            }
            Action::WorkspaceShowInvisible => self.set_workspace(|v| v.invisible),
            Action::WorkspaceSwitchToSheet(target) => {
                self.switch_to_sheet(self.resolve_sheet(target))
            }
            Action::WorkspaceSwitchInhabited { next } => self.switch_inhabited(next),

            Action::ViewCycle(cycle) => {
                let views = self.by_id(self.visible());
                self.cycle_through(views, cycle);
            }
            action => {
                let Some(window) = focused else {
                    return;
                };
                self.run_view_action(&window, action);
            }
        }
    }

    fn run_view_action(&mut self, window: &Window, action: Action) {
        match action {
            Action::ViewResize { direction, grow } => self.view_resize(window, direction, grow),
            Action::ViewMove(direction) => self.view_move(window, direction),
            Action::ViewMoveTo(position) => self.view_move_to(window, position),
            Action::ViewHide => {
                self.hide(window);
                self.update_workspace();
            }
            Action::ViewLower => self.lower(window),
            Action::ViewRaise => self.raise(window),
            Action::ViewOnly => {
                for other in self.visible() {
                    if other != *window {
                        self.hide(&other);
                    }
                }
                self.update_workspace();
            }
            Action::ViewPinToSheet(target) => self.pin_to_sheet(window, target),
            Action::ViewQuit => window.toplevel().unwrap().send_close(),
            Action::ViewResetGeometry => {
                self.layout_remove(window);
                let Some(view) = self.view_mut(window) else {
                    return;
                };
                view.maximized = Maximized::default();
                view.natural = None;
                let original = view.original;
                self.configure(window, original);
            }
            Action::ViewSnap(direction) => self.view_snap(window, direction),
            Action::ViewToggleFloating => {
                let floating = self.view_mut(window).map(|v| {
                    v.floating = !v.floating;
                    v.floating
                });
                if floating == Some(true) {
                    self.layout_remove(window);
                }
            }
            Action::ViewToggleInvisible => {
                let invisible = self.view_mut(window).map(|v| {
                    v.invisible = !v.invisible;
                    v.invisible
                });
                if invisible == Some(true) {
                    self.layout_remove(window);
                }
            }
            Action::ViewToggleMaximize {
                horizontal,
                vertical,
            } => self.toggle_maximize(window, horizontal, vertical),
            _ => unreachable!("not a view action"),
        }
    }
}

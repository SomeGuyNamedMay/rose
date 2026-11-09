//! A launcher modeled on Nyxt's prompt buffer: a prompt with the typed input, a heading naming
//! the source, and a table of suggestions narrowed down by fuzzy matching.

use std::{
    cmp::Reverse,
    collections::{BTreeSet, HashSet},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use smithay::{
    desktop::Window,
    input::keyboard::{Keysym, ModifiersState},
};

use crate::{
    action::{self, Action, MenuSource},
    config::Theme,
    files::Files,
    portal,
    text::{Canvas, Font},
    wm::{self, Mode},
    Rose,
};

/// What choosing an item does.
#[derive(Clone)]
pub enum Target {
    /// A shell command.
    Command(String),
    View(Window),
    Action(Action),
    /// Accepts a wallpaper from an application, or leaves the current one for `None`.
    Wallpaper(Option<Arc<portal::Request>>),
}

pub struct Item {
    pub columns: Vec<String>,
    pub target: Target,
}

/// An item that matches the input, with the matched character indices of each column.
struct Match {
    item: usize,
    highlights: Vec<Vec<usize>>,
}

pub enum MenuEvent {
    Continue,
    Close,
    Accept(Target),
}

pub struct Menu {
    source: MenuSource,
    items: Vec<Item>,
    input: String,
    /// Byte offset of the cursor in `input`.
    cursor: usize,
    matches: Vec<Match>,
    /// Index into `matches`.
    selected: usize,
    /// The first match shown.
    scroll: usize,
    pub lines: usize,
    /// Column widths in pixels, measured on the first paint.
    widths: Option<Vec<i32>>,
    /// The file manager, which replaces the list of items when browsing files.
    files: Option<Files>,
    /// Whether the menu changed since it was last painted.
    pub dirty: bool,
}

impl MenuSource {
    /// The prompt, the source's name and the column headers.
    fn labels(self) -> (&'static str, &'static str, &'static [&'static str]) {
        match self {
            Self::Applications => ("Launch", "Applications", &["Name", "Description"]),
            Self::Run => ("Run", "Programs in PATH", &["Program"]),
            Self::Views => (
                "Switch to view",
                "Views",
                &["Title", "App", "Sheet", "Group", "Mark"],
            ),
            Self::Actions => (
                "Execute action",
                "Actions",
                &["Action", "Binding", "Command"],
            ),
            Self::Layouts => ("Apply layout", "Layouts", &["Layout"]),
            Self::Wallpaper => (
                "Set wallpaper",
                "Requested by an application",
                &["Choice", "File"],
            ),
            // Painted by the file manager.
            Self::Files => ("Files", "Files", &[]),
        }
    }
}

impl Menu {
    pub fn new(source: MenuSource, items: Vec<Item>, lines: usize) -> Self {
        let mut menu = Self {
            source,
            items,
            input: String::new(),
            cursor: 0,
            matches: Vec::new(),
            selected: 0,
            scroll: 0,
            lines: lines.max(1),
            widths: None,
            files: None,
            dirty: true,
        };
        menu.refilter();
        menu
    }

    pub fn files(files: Files, lines: usize) -> Self {
        let mut menu = Self::new(MenuSource::Files, Vec::new(), lines);
        menu.files = Some(files);
        menu
    }

    /// Rows above the items: the prompt, the source heading and the column headers.
    pub const HEADER_ROWS: usize = 3;

    pub fn rows(&self) -> usize {
        Self::HEADER_ROWS + self.lines
    }

    fn refilter(&mut self) {
        let terms: Vec<Vec<char>> = self.input.split_whitespace().map(lowercase).collect();
        let mut scored: Vec<(i64, Match)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                score(&terms, &item.columns).map(|(score, highlights)| {
                    (
                        score,
                        Match {
                            item: i,
                            highlights,
                        },
                    )
                })
            })
            .collect();
        // Stable, so equal scores keep the source's order.
        scored.sort_by_key(|(score, _)| Reverse(*score));
        self.matches = scored.into_iter().map(|(_, m)| m).collect();
        self.selected = 0;
        self.scroll = 0;
    }

    fn select(&mut self, index: isize) {
        let n = self.matches.len() as isize;
        if n == 0 {
            return;
        }
        self.selected = index.clamp(0, n - 1) as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + self.lines {
            self.scroll = self.selected + 1 - self.lines;
        }
    }

    /// Moves the selection by `by` items, wrapping around at either end when moving by one.
    pub fn move_selection(&mut self, by: isize) {
        self.dirty = true;
        if let Some(files) = &mut self.files {
            files.move_selection(by);
            return;
        }
        let n = self.matches.len() as isize;
        let index = self.selected as isize + by;
        if by.abs() == 1 && n > 0 {
            self.select(index.rem_euclid(n));
        } else {
            self.select(index);
        }
    }

    fn selected_target(&self) -> Option<Target> {
        self.matches
            .get(self.selected)
            .map(|m| self.items[m.item].target.clone())
    }

    /// Accepts the item shown on `row`, counting from the first item row.
    pub fn accept_row(&mut self, row: usize) -> Option<Target> {
        if let Some(files) = &mut self.files {
            self.dirty = true;
            return files.accept_row(row);
        }
        let index = self.scroll + row;
        (index < self.matches.len()).then(|| {
            self.selected = index;
            self.selected_target().unwrap()
        })
    }

    /// The typed input as a command, for programs that aren't in the list or need arguments.
    fn input_command(&self) -> Option<Target> {
        let input = self.input.trim();
        (self.source == MenuSource::Run && !input.is_empty())
            .then(|| Target::Command(input.to_string()))
    }

    fn prev_boundary(&self) -> usize {
        self.input[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next_boundary(&self) -> usize {
        self.input[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }

    /// Where the word before the cursor starts.
    fn word_start(&self) -> usize {
        let before = self.input[..self.cursor].trim_end();
        before
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8())
    }

    pub fn key(&mut self, sym: Keysym, mods: &ModifiersState) -> MenuEvent {
        self.dirty = true;
        if let Some(files) = &mut self.files {
            return files.key(sym, mods, self.lines);
        }
        let input = self.input.clone();
        let page = self.lines as isize;

        match sym {
            Keysym::Escape => return MenuEvent::Close,
            Keysym::g | Keysym::c | Keysym::bracketleft if mods.ctrl => return MenuEvent::Close,
            Keysym::Return | Keysym::KP_Enter if mods.shift => {
                return self
                    .input_command()
                    .map_or(MenuEvent::Continue, MenuEvent::Accept)
            }
            Keysym::Return | Keysym::KP_Enter => {
                return match self.selected_target().or_else(|| self.input_command()) {
                    Some(target) => MenuEvent::Accept(target),
                    None => MenuEvent::Continue,
                };
            }
            Keysym::j | Keysym::m if mods.ctrl => {
                return self.key(Keysym::Return, &Default::default())
            }

            Keysym::Down | Keysym::Tab => self.move_selection(1),
            Keysym::n if mods.ctrl => self.move_selection(1),
            Keysym::Up | Keysym::ISO_Left_Tab => self.move_selection(-1),
            Keysym::p if mods.ctrl => self.move_selection(-1),
            Keysym::Page_Down => self.move_selection(page),
            Keysym::Page_Up => self.move_selection(-page),
            Keysym::Home => self.select(0),
            Keysym::End => self.select(self.matches.len() as isize - 1),

            Keysym::Left => self.cursor = self.prev_boundary(),
            Keysym::b if mods.ctrl => self.cursor = self.prev_boundary(),
            Keysym::Right => self.cursor = self.next_boundary(),
            Keysym::f if mods.ctrl => self.cursor = self.next_boundary(),
            Keysym::a if mods.ctrl => self.cursor = 0,
            Keysym::e if mods.ctrl => self.cursor = self.input.len(),

            Keysym::BackSpace if mods.ctrl => {
                let start = self.word_start();
                self.input.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            Keysym::w if mods.ctrl => return self.key(Keysym::BackSpace, mods),
            Keysym::BackSpace => {
                let start = self.prev_boundary();
                self.input.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            Keysym::h if mods.ctrl => return self.key(Keysym::BackSpace, &Default::default()),
            Keysym::Delete => {
                let end = self.next_boundary();
                self.input.replace_range(self.cursor..end, "");
            }
            Keysym::d if mods.ctrl => return self.key(Keysym::Delete, &Default::default()),
            Keysym::u if mods.ctrl => {
                self.input.replace_range(..self.cursor, "");
                self.cursor = 0;
            }
            Keysym::k if mods.ctrl => self.input.truncate(self.cursor),

            _ => {
                if let Some(c) = sym
                    .key_char()
                    .filter(|c| !c.is_control() && !mods.ctrl && !mods.alt && !mods.logo)
                {
                    self.input.insert(self.cursor, c);
                    self.cursor += c.len_utf8();
                }
            }
        }

        if self.input != input {
            self.refilter();
        }
        MenuEvent::Continue
    }

    /// Paints the menu, `row` pixels per row.
    pub fn paint(&mut self, canvas: &mut Canvas, font: &mut Font, theme: &Theme, row: i32) {
        self.dirty = false;
        if let Some(files) = &mut self.files {
            files.paint(canvas, font, theme, row, self.lines);
            return;
        }
        let (prompt, source, headers) = self.source.labels();
        let width = canvas.width;
        let pad = font.width(" ").max(4);
        let slant = row / 2;
        let line_height = font.line_height;
        let text_y = |r: usize| r as i32 * row + (row - line_height) / 2;

        canvas.fill((0, 0, width, canvas.height), theme.background);

        // The prompt line: the prompt, the match count and the input.
        let prompt_end = pad + font.width(prompt) + pad;
        canvas.fill_slanted((0, 0), (prompt_end, slant), (0, row), theme.primary);
        canvas.text(font, pad, text_y(0), prompt, theme.on_primary);
        let count = format!("[{}/{}]", self.matches.len(), self.items.len());
        let input_x = canvas.text(
            font,
            prompt_end + slant + pad,
            text_y(0),
            &count,
            theme.secondary,
        ) + pad;
        canvas.text(font, input_x, text_y(0), &self.input, theme.text);
        let cursor_x = input_x + font.width(&self.input[..self.cursor]);
        canvas.fill((cursor_x, text_y(0), 2, line_height), theme.accent);

        // The source heading and the column headers.
        canvas.fill((0, row, width, row), theme.secondary);
        canvas.text(font, pad, text_y(1), source, theme.on_secondary);
        canvas.fill((0, 2 * row, width, row), theme.primary);

        let widths = self.widths.get_or_insert_with(|| {
            // Wide enough for every item, but leaving room for the other columns.
            let cap = width * 2 / 5;
            (0..headers.len())
                .map(|i| {
                    self.items
                        .iter()
                        .filter_map(|item| item.columns.get(i))
                        .chain(std::iter::once(&headers[i].to_string()))
                        .map(|text| font.width(text))
                        .max()
                        .unwrap_or(0)
                        .min(cap)
                })
                .collect()
        });
        let gap = 3 * pad;
        let mut columns = Vec::with_capacity(widths.len());
        let mut x = pad;
        for (i, w) in widths.iter().enumerate() {
            let w = if i + 1 == widths.len() {
                width - x - pad
            } else {
                *w
            };
            columns.push((x, w.max(0)));
            x += w + gap;
        }
        for (header, (x, w)) in headers.iter().zip(&columns) {
            let header = font.truncate(header, *w);
            canvas.text(font, *x, text_y(2), &header, theme.on_primary);
        }

        // The items.
        let shown = self
            .matches
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(self.lines);
        for (r, (index, m)) in shown.enumerate() {
            let r = Self::HEADER_ROWS + r;
            let selected = index == self.selected;
            let (text, matched) = if selected {
                canvas.fill((0, r as i32 * row, width, row), theme.accent);
                (theme.on_accent, theme.on_primary)
            } else {
                (theme.text, theme.accent)
            };
            let item = &self.items[m.item];
            for (i, (x, w)) in columns.iter().enumerate() {
                let Some(cell) = item.columns.get(i) else {
                    continue;
                };
                let cell = font.truncate(cell, *w);
                let highlights = &m.highlights[i];
                canvas.text_with(font, *x, text_y(r), &cell, |c| {
                    if highlights.contains(&c) {
                        matched
                    } else {
                        text
                    }
                });
            }
        }
    }
}

pub fn lowercase(s: &str) -> Vec<char> {
    // One char each, so indices still line up with the original text.
    s.chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

/// Scores an item against the input's terms, each of which has to match one of the columns.
/// Returns the score and the matched character indices of each column.
pub fn score(terms: &[Vec<char>], columns: &[String]) -> Option<(i64, Vec<Vec<usize>>)> {
    let columns: Vec<Vec<char>> = columns.iter().map(|c| lowercase(c)).collect();
    let mut highlights = vec![Vec::new(); columns.len()];
    let mut total = 0;
    for term in terms {
        let (score, column, indices) = columns
            .iter()
            .enumerate()
            .filter_map(|(i, text)| {
                // Earlier columns are the more important ones.
                term_score(term, text).map(|(score, indices)| (score - 5 * i as i64, i, indices))
            })
            .max_by_key(|(score, ..)| *score)?;
        total += score;
        highlights[column].extend(indices);
    }
    Some((total, highlights))
}

fn is_word_start(text: &[char], i: usize) -> bool {
    i == 0 || !text[i - 1].is_alphanumeric()
}

/// Matches a term as a substring, preferring word starts, or else as a subsequence.
fn term_score(term: &[char], text: &[char]) -> Option<(i64, Vec<usize>)> {
    let n = term.len();
    let substring = (0..(text.len() + 1).saturating_sub(n))
        .filter(|&i| text[i..i + n] == *term)
        .max_by_key(|&i| (is_word_start(text, i), Reverse(i)));
    if let Some(i) = substring {
        let mut score = 100 - i.min(50) as i64;
        if is_word_start(text, i) {
            score += 50;
        }
        if i == 0 {
            score += 50;
        }
        if n == text.len() {
            score += 50;
        }
        return Some((score, (i..i + n).collect()));
    }

    let mut indices = Vec::with_capacity(n);
    let mut from = 0;
    for c in term {
        let i = from + text[from..].iter().position(|t| t == c)?;
        indices.push(i);
        from = i + 1;
    }
    let spread = (indices[n - 1] - indices[0] + 1 - n) as i64;
    let starts = indices.iter().filter(|&&i| is_word_start(text, i)).count() as i64;
    Some(((50 - spread).max(1) + 10 * starts, indices))
}

// Sources

/// Executable names in `PATH`, sorted.
fn programs() -> Vec<String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut names = BTreeSet::new();
    for dir in std::env::split_paths(&path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let executable = std::fs::metadata(entry.path())
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
            if executable {
                names.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    names.into_iter().collect()
}

struct DesktopEntry {
    name: String,
    comment: String,
    exec: String,
    terminal: bool,
}

/// Directories holding desktop entries, most important first.
fn application_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".local/share")));
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    data_home
        .into_iter()
        .chain(data_dirs.split(':').map(PathBuf::from))
        .map(|d| d.join("applications"))
        .collect()
}

/// Desktop entries of applications, sorted by name. Earlier directories override later ones.
fn applications() -> Vec<DesktopEntry> {
    fn walk(dir: &Path, id: &str, seen: &mut HashSet<String>, out: &mut Vec<DesktopEntry>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            // Desktop file ids join subdirectories with `-`.
            let file_id = format!("{id}{name}");
            if path.is_dir() {
                walk(&path, &format!("{file_id}-"), seen, out);
            } else if name.ends_with(".desktop") && seen.insert(file_id) {
                if let Some(entry) = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| parse_desktop_entry(&text))
                {
                    out.push(entry);
                }
            }
        }
    }

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for dir in application_dirs() {
        walk(&dir, "", &mut seen, &mut out);
    }
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

/// Parses a desktop entry, returning `None` for entries that shouldn't be shown.
fn parse_desktop_entry(text: &str) -> Option<DesktopEntry> {
    let mut in_entry = false;
    let (mut name, mut comment, mut generic, mut exec) = (None, None, None, None);
    let mut terminal = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        let Some((key, value)) = line.split_once('=').filter(|_| in_entry) else {
            continue;
        };
        let value = unescape(value.trim());
        match key.trim() {
            "Type" if value != "Application" => return None,
            "NoDisplay" | "Hidden" if value == "true" => return None,
            "Name" => name = Some(value),
            "Comment" => comment = Some(value),
            "GenericName" => generic = Some(value),
            "Exec" => exec = Some(value),
            "Terminal" => terminal = value == "true",
            _ => {}
        }
    }
    Some(DesktopEntry {
        name: name?,
        comment: comment.or(generic).unwrap_or_default(),
        exec: strip_field_codes(&exec?),
        terminal,
    })
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Removes `%f`, `%U` and the like from an `Exec` value, since no files are being opened.
fn strip_field_codes(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
        } else if let Some('%') = chars.next() {
            out.push('%');
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Rose {
    fn menu_items(&self, source: MenuSource) -> Vec<Item> {
        match source {
            MenuSource::Applications => {
                let terminal = self.config.actions.get("terminal");
                applications()
                    .into_iter()
                    .map(|e| {
                        let command = match terminal.filter(|_| e.terminal) {
                            Some(terminal) => format!("{terminal} -e {}", e.exec),
                            None => e.exec,
                        };
                        Item {
                            columns: vec![e.name, e.comment],
                            target: Target::Command(command),
                        }
                    })
                    .collect()
            }
            MenuSource::Run => programs()
                .into_iter()
                .map(|name| Item {
                    target: Target::Command(name.clone()),
                    columns: vec![name],
                })
                .collect(),
            MenuSource::Views => self
                .views
                .iter()
                .rev()
                .filter(|v| v.placed)
                .map(|v| {
                    let mark = self
                        .marks
                        .iter()
                        .find(|(_, w)| **w == v.window)
                        .map(|(m, _)| m.to_string())
                        .unwrap_or_default();
                    Item {
                        columns: vec![
                            wm::title(&v.window),
                            wm::app_id(&v.window),
                            v.sheet.to_string(),
                            v.group.clone(),
                            mark,
                        ],
                        target: Target::View(v.window.clone()),
                    }
                })
                .collect(),
            MenuSource::Actions => {
                let binding = |action: &Action| {
                    self.config
                        .keyboard_bindings
                        .iter()
                        .find(|(_, a)| a == action)
                        .map(|(b, _)| b.to_string())
                        .unwrap_or_default()
                };
                let mut commands: Vec<_> = self.config.actions.iter().collect();
                commands.sort();
                let commands = commands.into_iter().map(|(name, command)| {
                    let action = Action::Command(name.clone());
                    Item {
                        columns: vec![format!("action-{name}"), binding(&action), command.clone()],
                        target: Target::Action(action),
                    }
                });
                let mut layouts: Vec<_> = self
                    .config
                    .layouts
                    .keys()
                    .filter(|r| r.is_ascii_lowercase())
                    .collect();
                layouts.sort();
                let layouts = layouts.into_iter().map(|r| format!("layout-apply-{r}"));
                let builtin = action::all_names()
                    .into_iter()
                    .chain(layouts)
                    .filter_map(|name| {
                        let action: Action = name.parse().ok()?;
                        Some(Item {
                            columns: vec![name, binding(&action), String::new()],
                            target: Target::Action(action),
                        })
                    });
                commands.chain(builtin).collect()
            }
            MenuSource::Layouts => {
                let mut layouts: Vec<_> = self.config.layouts.iter().collect();
                // Named registers first, then the sheet defaults.
                layouts.sort_by_key(|(r, _)| (r.is_ascii_digit(), **r));
                layouts
                    .into_iter()
                    .map(|(&register, layout)| Item {
                        columns: vec![layout.describe()],
                        target: Target::Action(Action::LayoutApply(register)),
                    })
                    .collect()
            }
            // Files has its own painting, and Wallpaper its own items.
            MenuSource::Files | MenuSource::Wallpaper => Vec::new(),
        }
    }

    pub fn open_menu(&mut self, source: MenuSource) {
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        let lines = self.config.menu.lines;
        self.menu = Some(if source == MenuSource::Files {
            // Back where the file manager was last closed.
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let dir = [self.files_dir.clone(), home]
                .into_iter()
                .flatten()
                .find(|d| d.is_dir())
                .unwrap_or_else(|| "/".into());
            let opener = self.config.menu.opener.clone();
            let terminal = self.config.actions.get("terminal").cloned();
            Menu::files(Files::new(dir, opener, terminal), lines)
        } else {
            Menu::new(source, self.menu_items(source), lines)
        });
        self.mode = Mode::Menu;
    }

    pub fn close_menu(&mut self) {
        if let Some(files) = self.menu.as_ref().and_then(|m| m.files.as_ref()) {
            self.files_dir = Some(files.dir().to_path_buf());
        }
        self.menu = None;
        if matches!(self.mode, Mode::Menu) {
            self.mode = Mode::Normal;
        }
    }

    pub fn handle_menu_key(&mut self, sym: Keysym, mods: &ModifiersState) {
        let Some(menu) = &mut self.menu else {
            self.close_menu();
            return;
        };
        match menu.key(sym, mods) {
            MenuEvent::Continue => {}
            MenuEvent::Close => self.close_menu(),
            MenuEvent::Accept(target) => {
                self.close_menu();
                self.run_target(target);
            }
        }
    }

    pub fn run_target(&mut self, target: Target) {
        match target {
            Target::Command(command) => wm::spawn(&command),
            Target::View(window) => {
                if self.view(&window).is_some() {
                    self.show_view(&window, true);
                }
            }
            Target::Action(action) => self.run_action(action, None),
            Target::Wallpaper(Some(request)) => self.accept_wallpaper(&request),
            Target::Wallpaper(None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(input: &str, items: &[&str]) -> Vec<String> {
        let items = items
            .iter()
            .map(|s| Item {
                columns: vec![s.to_string()],
                target: Target::Command(s.to_string()),
            })
            .collect();
        let mut menu = Menu::new(MenuSource::Run, items, 10);
        for c in input.chars() {
            menu.key(Keysym::from_char(c), &ModifiersState::default());
        }
        menu.matches
            .iter()
            .map(|m| menu.items[m.item].columns[0].clone())
            .collect()
    }

    #[test]
    fn fuzzy_matching() {
        let items = [
            "firefox",
            "foot",
            "gnome-terminal",
            "nautilus",
            "footclient",
        ];
        assert_eq!(matches("foot", &items), ["foot", "footclient"]);
        assert_eq!(matches("ff", &items), ["firefox"]);
        assert_eq!(matches("term", &items), ["gnome-terminal"]);
        assert_eq!(matches("gn te", &items), ["gnome-terminal"]);
        assert_eq!(matches("", &items).len(), items.len());
        assert!(matches("xyz", &items).is_empty());
    }

    #[test]
    fn editing() {
        let mut menu = Menu::new(MenuSource::Run, Vec::new(), 10);
        let none = ModifiersState::default();
        let ctrl = ModifiersState {
            ctrl: true,
            ..Default::default()
        };
        for c in "foot -e htop".chars() {
            menu.key(Keysym::from_char(c), &none);
        }
        menu.key(Keysym::w, &ctrl);
        assert_eq!(menu.input, "foot -e ");
        menu.key(Keysym::a, &ctrl);
        menu.key(Keysym::Delete, &none);
        assert_eq!(menu.input, "oot -e ");
        assert!(matches!(
            menu.key(Keysym::Return, &none),
            MenuEvent::Accept(Target::Command(c)) if c == "oot -e"
        ));
    }

    #[test]
    fn desktop_entries() {
        let entry = parse_desktop_entry(
            "[Desktop Entry]\nType=Application\nName=Files\nGenericName=File Manager\n\
             Exec=nautilus --new-window %U\nTerminal=false\n\n[Desktop Action new]\nName=New",
        )
        .unwrap();
        assert_eq!(entry.name, "Files");
        assert_eq!(entry.comment, "File Manager");
        assert_eq!(entry.exec, "nautilus --new-window");
        assert!(parse_desktop_entry("[Desktop Entry]\nName=X\nExec=x\nNoDisplay=true").is_none());
    }
}

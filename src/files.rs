//! A file manager in the menu, modeled on ranger: the parent directory, the current directory and
//! a preview of the selected entry side by side, driven by vi-like keys.

use std::{
    cmp::Reverse,
    collections::HashMap,
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use smithay::input::keyboard::{Keysym, ModifiersState};

use crate::{
    config::{Color, Theme},
    menu::{self, MenuEvent, Target},
    text::{Canvas, Font},
};

/// How much of a file is read to preview it.
const PREVIEW_BYTES: usize = 32 * 1024;

struct Entry {
    name: String,
    path: PathBuf,
    /// A directory, or a link to one.
    dir: bool,
    link: bool,
    mode: u32,
    size: u64,
}

impl Entry {
    /// Like `ls -l`: the type, then the permissions.
    fn permissions(&self) -> String {
        let kind = if self.link {
            'l'
        } else if self.dir {
            'd'
        } else {
            '-'
        };
        let mut out = String::from(kind);
        for shift in [6, 3, 0] {
            let bits = self.mode >> shift;
            out.push(if bits & 4 != 0 { 'r' } else { '-' });
            out.push(if bits & 2 != 0 { 'w' } else { '-' });
            out.push(if bits & 1 != 0 { 'x' } else { '-' });
        }
        out
    }
}

enum Preview {
    None,
    Entries(Vec<Entry>),
    Text(Vec<String>),
    /// A description of something that can't be shown, or an error.
    Info(String),
}

pub struct Files {
    dir: PathBuf,
    entries: Vec<Entry>,
    /// Why `dir` couldn't be read.
    error: Option<String>,
    /// Entries matching the filter, as indices into `entries` with the matched character indices.
    shown: Vec<(usize, Vec<usize>)>,
    /// Index into `shown`.
    selected: usize,
    /// The first entry shown.
    scroll: usize,
    parent: Vec<Entry>,
    preview: Preview,
    filter: String,
    /// Whether keys are typed into the filter, after `/`.
    typing: bool,
    hidden: bool,
    /// The first key of a two-key command like `gg`.
    pending: Option<Keysym>,
    /// The entry last selected in each directory visited.
    last_selected: HashMap<PathBuf, String>,
    opener: String,
    terminal: Option<String>,
}

impl Files {
    pub fn new(dir: PathBuf, opener: String, terminal: Option<String>) -> Self {
        let mut files = Self {
            dir,
            entries: Vec::new(),
            error: None,
            shown: Vec::new(),
            selected: 0,
            scroll: 0,
            parent: Vec::new(),
            preview: Preview::None,
            filter: String::new(),
            typing: false,
            hidden: false,
            pending: None,
            last_selected: HashMap::new(),
            opener,
            terminal,
        };
        files.load(None);
        files
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reads the current and parent directories, selecting `select` if it is there.
    fn load(&mut self, select: Option<&str>) {
        (self.entries, self.error) = match read_entries(&self.dir, self.hidden) {
            Ok(entries) => (entries, None),
            Err(err) => (Vec::new(), Some(err)),
        };
        self.parent = self
            .dir
            .parent()
            .and_then(|p| read_entries(p, self.hidden).ok())
            .unwrap_or_default();
        let select = select
            .map(str::to_string)
            .or_else(|| self.last_selected.get(&self.dir).cloned());
        self.refilter(select.as_deref());
    }

    /// Narrows the entries down to those matching the filter, keeping their order.
    fn refilter(&mut self, select: Option<&str>) {
        let terms: Vec<Vec<char>> = self
            .filter
            .split_whitespace()
            .map(menu::lowercase)
            .collect();
        self.shown = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let (_, highlights) = menu::score(&terms, std::slice::from_ref(&e.name))?;
                Some((i, highlights.into_iter().next().unwrap_or_default()))
            })
            .collect();
        let index = select
            .and_then(|name| {
                self.shown
                    .iter()
                    .position(|(i, _)| self.entries[*i].name == name)
            })
            .unwrap_or(0);
        self.scroll = 0;
        self.select(index);
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.shown
            .get(self.selected)
            .map(|(i, _)| &self.entries[*i])
    }

    fn select(&mut self, index: usize) {
        self.selected = index.min(self.shown.len().saturating_sub(1));
        self.preview = match self.selected_entry() {
            None => Preview::None,
            Some(entry) if entry.dir => match read_entries(&entry.path, self.hidden) {
                Ok(entries) if entries.is_empty() => Preview::Info("empty".into()),
                Ok(entries) => Preview::Entries(entries),
                Err(err) => Preview::Info(err),
            },
            Some(entry) => preview_file(&entry.path),
        };
    }

    fn move_by(&mut self, by: isize) {
        self.select(self.selected.saturating_add_signed(by));
    }

    /// Changes to `dir`, remembering the selection in the one being left.
    fn cd(&mut self, dir: PathBuf, select: Option<String>) {
        if let Some(entry) = self.selected_entry() {
            self.last_selected
                .insert(self.dir.clone(), entry.name.clone());
        }
        self.dir = dir;
        self.filter.clear();
        self.typing = false;
        self.load(select.as_deref());
    }

    fn cd_parent(&mut self) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            let name = self
                .dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
            self.cd(parent, name);
        }
    }

    fn toggle_hidden(&mut self) {
        self.hidden = !self.hidden;
        let name = self.selected_entry().map(|e| e.name.clone());
        self.load(name.as_deref());
    }

    /// Enters the selected directory, or opens the selected file.
    fn open_selected(&mut self) -> MenuEvent {
        match self.selected_entry() {
            Some(entry) if entry.dir => {
                let path = entry.path.clone();
                self.cd(path, None);
                MenuEvent::Continue
            }
            Some(entry) => MenuEvent::Accept(Target::Command(format!(
                "{} {}",
                self.opener,
                quote(&entry.path)
            ))),
            None => MenuEvent::Continue,
        }
    }

    /// Accepts the entry shown on `row`, counting from the first entry row. Entering a
    /// directory keeps the menu open, so it returns `None`.
    pub fn accept_row(&mut self, row: usize) -> Option<Target> {
        let index = self.scroll + row;
        if index >= self.shown.len() {
            return None;
        }
        self.select(index);
        match self.open_selected() {
            MenuEvent::Accept(target) => Some(target),
            _ => None,
        }
    }

    pub fn move_selection(&mut self, by: isize) {
        self.move_by(by);
    }

    pub fn key(&mut self, sym: Keysym, mods: &ModifiersState, lines: usize) -> MenuEvent {
        let page = lines as isize;
        if mods.ctrl {
            match sym {
                Keysym::g | Keysym::c | Keysym::bracketleft => return MenuEvent::Close,
                Keysym::n => self.move_by(1),
                Keysym::p => self.move_by(-1),
                Keysym::d => self.move_by(page / 2),
                Keysym::u => self.move_by(-page / 2),
                Keysym::f => self.move_by(page),
                Keysym::b => self.move_by(-page),
                Keysym::h => self.toggle_hidden(),
                _ => {}
            }
            return MenuEvent::Continue;
        }

        if self.typing {
            let filter = self.filter.clone();
            match sym {
                Keysym::Escape => {
                    self.filter.clear();
                    self.typing = false;
                }
                Keysym::Return | Keysym::KP_Enter => self.typing = false,
                Keysym::BackSpace => {
                    if self.filter.pop().is_none() {
                        self.typing = false;
                    }
                }
                Keysym::Down => self.move_by(1),
                Keysym::Up => self.move_by(-1),
                _ => {
                    if let Some(c) = sym
                        .key_char()
                        .filter(|c| !c.is_control() && !mods.alt && !mods.logo)
                    {
                        self.filter.push(c);
                    }
                }
            }
            if self.filter != filter {
                let name = self.selected_entry().map(|e| e.name.clone());
                self.refilter(name.as_deref());
            }
            return MenuEvent::Continue;
        }

        if let Some(first) = self.pending.take() {
            match (first, sym) {
                (Keysym::g, Keysym::g) => self.select(0),
                (Keysym::g, Keysym::h) => self.cd(home(), None),
                (Keysym::g, Keysym::slash) => self.cd("/".into(), None),
                (Keysym::z, Keysym::h) => self.toggle_hidden(),
                _ => {}
            }
            return MenuEvent::Continue;
        }

        match sym {
            Keysym::Escape if !self.filter.is_empty() => {
                self.filter.clear();
                let name = self.selected_entry().map(|e| e.name.clone());
                self.refilter(name.as_deref());
            }
            Keysym::Escape | Keysym::q => return MenuEvent::Close,
            Keysym::j | Keysym::Down => self.move_by(1),
            Keysym::k | Keysym::Up => self.move_by(-1),
            Keysym::J | Keysym::Page_Down => self.move_by(page),
            Keysym::K | Keysym::Page_Up => self.move_by(-page),
            Keysym::h | Keysym::Left => self.cd_parent(),
            Keysym::l | Keysym::Right | Keysym::Return | Keysym::KP_Enter => {
                return self.open_selected()
            }
            Keysym::G | Keysym::End => self.select(usize::MAX),
            Keysym::Home => self.select(0),
            Keysym::g | Keysym::z => self.pending = Some(sym),
            Keysym::BackSpace => self.toggle_hidden(),
            Keysym::asciitilde => self.cd(home(), None),
            Keysym::slash => {
                self.filter.clear();
                self.typing = true;
            }
            // A shell in the current directory.
            Keysym::S => {
                if let Some(terminal) = &self.terminal {
                    return MenuEvent::Accept(Target::Command(format!(
                        "cd {} && exec {terminal}",
                        quote(&self.dir)
                    )));
                }
            }
            _ => {}
        }
        MenuEvent::Continue
    }

    /// Paints the file manager, `row` pixels per row, with `lines` rows of entries.
    pub fn paint(
        &mut self,
        canvas: &mut Canvas,
        font: &mut Font,
        theme: &Theme,
        row: i32,
        lines: usize,
    ) {
        // Keep the selection in view, with the scroll only moving when it has to.
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + lines {
            self.scroll = self.selected + 1 - lines;
        }
        self.scroll = self.scroll.min(self.shown.len().saturating_sub(lines));

        let width = canvas.width;
        let pad = font.width(" ").max(4);
        let slant = row / 2;
        let line_height = font.line_height;
        let text_y = |r: usize| r as i32 * row + (row - line_height) / 2;

        canvas.fill((0, 0, width, canvas.height), theme.background);

        // The prompt line: the path of the selected entry, and the filter.
        let prompt = "Files";
        let prompt_end = pad + font.width(prompt) + pad;
        canvas.fill_slanted((0, 0), (prompt_end, slant), (0, row), theme.primary);
        canvas.text(font, pad, text_y(0), prompt, theme.on_primary);
        let mut dir = display_path(&self.dir);
        if !dir.ends_with('/') {
            dir.push('/');
        }
        let mut x = canvas.text(
            font,
            prompt_end + slant + pad,
            text_y(0),
            &dir,
            theme.accent,
        );
        if let Some(entry) = self.selected_entry() {
            x = canvas.text(font, x, text_y(0), &entry.name, theme.text);
        }
        if self.typing || !self.filter.is_empty() {
            let filter = format!("/{}", self.filter);
            let end = canvas.text(font, x + 3 * pad, text_y(0), &filter, theme.secondary);
            if self.typing {
                canvas.fill((end, text_y(0), 2, line_height), theme.accent);
            }
        }

        // Details of the selected entry, and the position in the directory.
        canvas.fill((0, row, width, row), theme.secondary);
        let details = match self.selected_entry() {
            Some(entry) => {
                let mut details = entry.permissions();
                if !entry.dir {
                    details += &format!("  {}", human_size(entry.size));
                }
                if let Some(target) = entry
                    .link
                    .then(|| fs::read_link(&entry.path).ok())
                    .flatten()
                {
                    details += &format!("  -> {}", target.display());
                }
                details
            }
            None => self.error.clone().unwrap_or_else(|| "empty".into()),
        };
        canvas.text(font, pad, text_y(1), &details, theme.on_secondary);
        let mut position = format!(
            "{}/{}",
            (self.selected + 1).min(self.shown.len()),
            self.shown.len()
        );
        if self.hidden {
            position = format!("hidden  {position}");
        }
        let position_x = width - pad - font.width(&position);
        canvas.text(font, position_x, text_y(1), &position, theme.on_secondary);

        // Three panes, sized like ranger's 1:3:4.
        let gap = 2 * pad;
        let available = width - 2 * pad - 2 * gap;
        let parent_x = pad;
        let parent_w = available / 8;
        let current_x = parent_x + parent_w + gap;
        let current_w = available * 3 / 8;
        let preview_x = current_x + current_w + gap;
        let preview_w = width - pad - preview_x;

        // The pane headers.
        canvas.fill((0, 2 * row, width, row), theme.primary);
        let parent_name = self.dir.parent().map(display_path).unwrap_or_default();
        let current_name = self
            .dir
            .file_name()
            .map_or_else(|| "/".into(), |n| n.to_string_lossy().into_owned());
        let preview_name = self
            .selected_entry()
            .map(|e| e.name.clone())
            .unwrap_or_default();
        for (x, w, name) in [
            (parent_x, parent_w, parent_name),
            (current_x, current_w, current_name),
            (preview_x, preview_w, preview_name),
        ] {
            let name = font.truncate(&name, w);
            canvas.text(font, x, text_y(2), &name, theme.on_primary);
        }

        let top = menu::Menu::HEADER_ROWS;
        let geometry = Geometry {
            row,
            top,
            lines,
            pad,
            text_y: &text_y,
        };

        let current_name = self.dir.file_name().map(|n| n.to_string_lossy());
        let parent_selected =
            current_name.and_then(|name| self.parent.iter().position(|e| e.name == name));
        paint_entries(
            canvas,
            font,
            theme,
            &geometry,
            Pane {
                x: parent_x,
                w: parent_w,
                entries: self.parent.iter().map(|e| (e, &[][..])).collect(),
                selected: parent_selected,
                scroll: centered(parent_selected, self.parent.len(), lines),
                focused: false,
                sizes: false,
            },
        );

        if let Some(error) = &self.error {
            let error = font.truncate(error, current_w);
            canvas.text(font, current_x, text_y(top), &error, theme.secondary);
        }
        paint_entries(
            canvas,
            font,
            theme,
            &geometry,
            Pane {
                x: current_x,
                w: current_w,
                entries: self
                    .shown
                    .iter()
                    .map(|(i, highlights)| (&self.entries[*i], &highlights[..]))
                    .collect(),
                selected: (!self.shown.is_empty()).then_some(self.selected),
                scroll: self.scroll,
                focused: true,
                sizes: true,
            },
        );

        match &self.preview {
            Preview::None => {}
            Preview::Entries(entries) => paint_entries(
                canvas,
                font,
                theme,
                &geometry,
                Pane {
                    x: preview_x,
                    w: preview_w,
                    entries: entries.iter().map(|e| (e, &[][..])).collect(),
                    selected: None,
                    scroll: 0,
                    focused: false,
                    sizes: true,
                },
            ),
            Preview::Text(text) => {
                for (r, line) in text.iter().take(lines).enumerate() {
                    let line = font.truncate(line, preview_w);
                    canvas.text(font, preview_x, text_y(top + r), &line, theme.text);
                }
            }
            Preview::Info(info) => {
                let info = font.truncate(info, preview_w);
                canvas.text(font, preview_x, text_y(top), &info, theme.secondary);
            }
        }
    }
}

struct Geometry<'a> {
    row: i32,
    /// The row of the first entry.
    top: usize,
    lines: usize,
    pad: i32,
    text_y: &'a dyn Fn(usize) -> i32,
}

struct Pane<'a> {
    x: i32,
    w: i32,
    /// Entries with the indices of their characters matching the filter.
    entries: Vec<(&'a Entry, &'a [usize])>,
    selected: Option<usize>,
    scroll: usize,
    /// Whether the selection is the one the keys move, rather than the directory it's in.
    focused: bool,
    /// Whether to show file sizes.
    sizes: bool,
}

fn paint_entries(canvas: &mut Canvas, font: &mut Font, theme: &Theme, g: &Geometry, pane: Pane) {
    let shown = pane
        .entries
        .iter()
        .enumerate()
        .skip(pane.scroll)
        .take(g.lines);
    for (r, (index, (entry, highlights))) in shown.enumerate() {
        let r = g.top + r;
        let y = (g.text_y)(r);
        let (text, matched): (Color, Color) = if pane.selected == Some(index) {
            let (background, text) = if pane.focused {
                (theme.accent, theme.on_accent)
            } else {
                (theme.secondary, theme.on_secondary)
            };
            canvas.fill(
                (pane.x - g.pad / 2, r as i32 * g.row, pane.w + g.pad, g.row),
                background,
            );
            (text, theme.on_primary)
        } else if entry.dir {
            (theme.accent, theme.on_primary)
        } else {
            (theme.text, theme.accent)
        };

        let mut name_w = pane.w;
        if pane.sizes && !entry.dir {
            let size = human_size(entry.size);
            let size_w = font.width(&size);
            canvas.text(font, pane.x + pane.w - size_w, y, &size, text);
            name_w -= size_w + g.pad;
        }
        let mut name = entry.name.clone();
        if entry.dir {
            name.push('/');
        }
        let name = font.truncate(&name, name_w.max(0));
        canvas.text_with(font, pane.x, y, &name, |c| {
            if highlights.contains(&c) {
                matched
            } else {
                text
            }
        });
    }
}

/// The scroll that puts `selected` in the middle of the pane.
fn centered(selected: Option<usize>, len: usize, lines: usize) -> usize {
    selected
        .unwrap_or(0)
        .saturating_sub(lines / 2)
        .min(len.saturating_sub(lines))
}

/// Directory entries, directories first and then by name.
fn read_entries(dir: &Path, hidden: bool) -> Result<Vec<Entry>, String> {
    let mut entries: Vec<Entry> = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !hidden && name.starts_with('.') {
                return None;
            }
            let path = e.path();
            let link = e.file_type().is_ok_and(|t| t.is_symlink());
            // Follows links, so links to directories can be entered; a broken link is shown as
            // itself.
            let metadata = fs::metadata(&path)
                .or_else(|_| fs::symlink_metadata(&path))
                .ok()?;
            Some(Entry {
                name,
                path,
                dir: metadata.is_dir(),
                link,
                mode: metadata.permissions().mode(),
                size: metadata.len(),
            })
        })
        .collect();
    entries.sort_by_cached_key(|e| (Reverse(e.dir), e.name.to_lowercase()));
    Ok(entries)
}

fn preview_file(path: &Path) -> Preview {
    let image = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .ok()
        .filter(|r| r.format().is_some())
        .and_then(|r| r.into_dimensions().ok());
    if let Some((w, h)) = image {
        return Preview::Info(format!("image, {w}x{h}"));
    }

    let mut bytes = Vec::new();
    let read =
        fs::File::open(path).and_then(|f| f.take(PREVIEW_BYTES as u64).read_to_end(&mut bytes));
    if let Err(err) = read {
        return Preview::Info(err.to_string());
    }
    if bytes.contains(&0) {
        return Preview::Info("binary".into());
    }
    Preview::Text(
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(|line| line.replace('\t', "    "))
            .collect(),
    )
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| "/".into(), PathBuf::from)
}

/// `path` with the home directory shortened to `~`.
fn display_path(path: &Path) -> String {
    let home = home();
    match path.strip_prefix(&home) {
        Ok(rest) if home != Path::new("/") => {
            if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", rest.display())
            }
        }
        _ => path.display().to_string(),
    }
}

/// Quotes `path` for `sh`.
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

fn human_size(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 || value >= 10.0 {
        format!("{}{}", value.round(), UNITS[unit])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> ModifiersState {
        ModifiersState::default()
    }

    /// A directory tree for the tests, removed when dropped.
    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("rose-files-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("src/deep")).unwrap();
            fs::create_dir_all(root.join("docs")).unwrap();
            fs::write(root.join("README.md"), "# Rose\n\tindented\n").unwrap();
            fs::write(root.join("Cargo.toml"), "[package]\n").unwrap();
            fs::write(root.join(".hidden"), "").unwrap();
            fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
            Self(root)
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names(files: &Files) -> Vec<&str> {
        files
            .shown
            .iter()
            .map(|(i, _)| files.entries[*i].name.as_str())
            .collect()
    }

    fn selected(files: &Files) -> &str {
        &files.selected_entry().unwrap().name
    }

    #[test]
    fn lists_directories_first_and_hides_dotfiles() {
        let tree = Tree::new("list");
        let mut files = Files::new(tree.0.clone(), "xdg-open".into(), None);
        assert_eq!(names(&files), ["docs", "src", "Cargo.toml", "README.md"]);
        files.key(Keysym::z, &none(), 10);
        files.key(Keysym::h, &none(), 10);
        assert_eq!(names(&files)[2], ".hidden");
    }

    #[test]
    fn navigates_like_ranger() {
        let tree = Tree::new("navigate");
        let mut files = Files::new(tree.0.clone(), "xdg-open".into(), Some("foot".into()));
        files.key(Keysym::j, &none(), 10);
        assert_eq!(selected(&files), "src");
        assert!(matches!(&files.preview, Preview::Entries(e) if e.len() == 2));

        files.key(Keysym::l, &none(), 10);
        assert_eq!(files.dir(), tree.0.join("src"));
        assert_eq!(selected(&files), "deep");
        files.key(Keysym::G, &none(), 10);
        assert_eq!(selected(&files), "main.rs");
        assert!(matches!(&files.preview, Preview::Text(t) if t[0] == "fn main() {}"));

        // Going up selects the directory we came from, and coming back restores the selection.
        files.key(Keysym::h, &none(), 10);
        assert_eq!(selected(&files), "src");
        files.key(Keysym::Right, &none(), 10);
        assert_eq!(selected(&files), "main.rs");

        assert!(matches!(
            files.key(Keysym::Return, &none(), 10),
            MenuEvent::Accept(Target::Command(c)) if c == format!("xdg-open '{}'", tree.0.join("src/main.rs").display())
        ));
        assert!(matches!(
            files.key(Keysym::S, &none(), 10),
            MenuEvent::Accept(Target::Command(c)) if c.starts_with("cd '") && c.ends_with("' && exec foot")
        ));
        assert!(matches!(
            files.key(Keysym::q, &none(), 10),
            MenuEvent::Close
        ));
    }

    #[test]
    fn filters_after_slash() {
        let tree = Tree::new("filter");
        let mut files = Files::new(tree.0.clone(), "xdg-open".into(), None);
        files.key(Keysym::slash, &none(), 10);
        for c in "rd".chars() {
            files.key(Keysym::from_char(c), &none(), 10);
        }
        assert_eq!(names(&files), ["README.md"]);
        // Return stops typing but keeps the filter, so keys navigate again.
        files.key(Keysym::Return, &none(), 10);
        assert!(!files.typing);
        assert_eq!(names(&files), ["README.md"]);
        // Escape clears the filter before closing.
        assert!(matches!(
            files.key(Keysym::Escape, &none(), 10),
            MenuEvent::Continue
        ));
        assert_eq!(names(&files).len(), 4);
        assert_eq!(selected(&files), "README.md");
    }

    #[test]
    fn helpers() {
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1536), "1.5K");
        assert_eq!(human_size(20 * 1024 * 1024), "20M");
        assert_eq!(quote(Path::new("it's")), r"'it'\''s'");
    }
}

//! TOML configuration, structured like hikari's config.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use smithay::input::keyboard::{xkb, Keysym, KeysymHandle, ModifiersState, XkbConfig};

use crate::{action::Action, layout::Layout};

/// Used when no config file exists, and as a reference for writing one.
pub const DEFAULT_CONFIG: &str = include_str!("../config.toml");

pub struct Config {
    pub ui: Ui,
    pub actions: HashMap<String, String>,
    pub keyboard_bindings: Vec<(KeyBinding, Action)>,
    pub mouse_bindings: Vec<(MouseBinding, Action)>,
    /// Layout registers `a`-`z`, plus `0`-`9` as the default layout of each sheet.
    pub layouts: HashMap<char, Layout>,
    /// Commands run when selecting a mark that isn't bound to a view.
    pub marks: HashMap<char, String>,
    /// Rules for new views, keyed by app id.
    pub views: HashMap<String, ViewRule>,
    pub keyboard: KeyboardConfig,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    ui: Ui,
    actions: HashMap<String, String>,
    bindings: RawBindings,
    layouts: toml::Table,
    marks: HashMap<char, String>,
    views: HashMap<String, ViewRule>,
    inputs: Inputs,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawBindings {
    keyboard: HashMap<String, String>,
    mouse: HashMap<String, String>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Inputs {
    keyboard: KeyboardConfig,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// Border width around views.
    pub border: i32,
    /// Space between tiled views and around the output edge.
    pub gap: i32,
    /// Pixels that keyboard move and resize actions change a view by.
    pub step: i32,
    pub colorscheme: Colorscheme,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            border: 1,
            gap: 5,
            step: 100,
            colorscheme: Colorscheme::default(),
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Colorscheme {
    /// Behind all views.
    pub background: Color,
    /// Border of the focused view.
    pub active: Color,
    /// Border of unfocused views.
    pub inactive: Color,
    /// Border of views in the same group as the focused view.
    pub grouped: Color,
    /// Border of the focused view while a mode is waiting for input.
    pub insert: Color,
    /// Border of the focused view when the input would overwrite something, like a taken mark.
    pub conflict: Color,
}

impl Default for Colorscheme {
    fn default() -> Self {
        Self {
            background: Color::rgb(0x282C34),
            active: Color::rgb(0xFFFFFF),
            inactive: Color::rgb(0x465457),
            grouped: Color::rgb(0xFDAF53),
            insert: Color::rgb(0xE3C3FA),
            conflict: Color::rgb(0xED6B32),
        }
    }
}

/// An opaque color, written as `0xRRGGBB` or `"#RRGGBB"`.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(try_from = "RawColor")]
pub struct Color(pub [f32; 4]);

#[derive(Deserialize)]
#[serde(untagged)]
enum RawColor {
    Int(i64),
    Str(String),
}

impl Color {
    fn rgb(hex: u32) -> Self {
        let channel = |shift: u32| ((hex >> shift) & 0xFF) as f32 / 255.0;
        Self([channel(16), channel(8), channel(0), 1.0])
    }
}

impl TryFrom<RawColor> for Color {
    type Error = String;

    fn try_from(raw: RawColor) -> Result<Self, String> {
        let hex = match raw {
            RawColor::Int(n) => u32::try_from(n).ok(),
            RawColor::Str(s) => {
                let digits = s
                    .strip_prefix('#')
                    .or_else(|| s.strip_prefix("0x"))
                    .unwrap_or(&s);
                u32::from_str_radix(digits, 16).ok()
            }
        };
        hex.filter(|h| *h <= 0xFFFFFF)
            .map(Self::rgb)
            .ok_or_else(|| "colors must be written as 0xRRGGBB or \"#RRGGBB\"".to_string())
    }
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ViewRule {
    pub sheet: Option<u8>,
    pub group: Option<String>,
    pub mark: Option<char>,
    pub floating: bool,
    pub invisible: bool,
    pub position: Option<ViewPosition>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewPosition {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct KeyboardConfig {
    pub xkb: Xkb,
    /// Milliseconds before a held key starts repeating.
    pub repeat_delay: i32,
    /// Repeats per second.
    pub repeat_rate: i32,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            xkb: Xkb::default(),
            repeat_delay: 600,
            repeat_rate: 25,
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Xkb {
    pub rules: String,
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: Option<String>,
}

impl KeyboardConfig {
    pub fn xkb_config(&self) -> XkbConfig<'_> {
        XkbConfig {
            rules: &self.xkb.rules,
            model: &self.xkb.model,
            layout: &self.xkb.layout,
            variant: &self.xkb.variant,
            options: self.xkb.options.clone(),
        }
    }
}

/// Modifiers in hikari's notation: `L`ogo, `S`hift, `C`ontrol, `A`lt and AltGr (`5`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    logo: bool,
    shift: bool,
    ctrl: bool,
    alt: bool,
    altgr: bool,
}

impl Modifiers {
    fn parse(s: &str) -> Result<Self, String> {
        let mut mods = Self::default();
        for c in s.chars() {
            match c {
                'L' => mods.logo = true,
                'S' => mods.shift = true,
                'C' => mods.ctrl = true,
                'A' => mods.alt = true,
                '5' => mods.altgr = true,
                '0' => {}
                _ => return Err(format!("unknown modifier `{c}`")),
            }
        }
        Ok(mods)
    }

    pub fn from_state(state: &ModifiersState) -> Self {
        Self {
            logo: state.logo,
            shift: state.shift,
            ctrl: state.ctrl,
            alt: state.alt,
            altgr: state.iso_level3_shift,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// `"A+Return"`: matched against the unshifted keysym.
    Sym(Keysym),
    /// `"A-36"`: matched against the xkb keycode.
    Code(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyBinding {
    pub mods: Modifiers,
    pub key: Key,
}

impl KeyBinding {
    fn parse(s: &str) -> Result<Self, String> {
        let (mods, separator, key) = split_binding(s)?;
        let key = if separator == '+' {
            Key::Sym(parse_keysym(key)?)
        } else {
            Key::Code(
                key.parse()
                    .map_err(|_| format!("invalid keycode `{key}`"))?,
            )
        };
        Ok(Self { mods, key })
    }

    pub fn matches(&self, mods: &ModifiersState, handle: &KeysymHandle<'_>) -> bool {
        if self.mods != Modifiers::from_state(mods) {
            return false;
        }
        match self.key {
            Key::Sym(sym) => {
                handle.raw_latin_sym_or_raw_current_sym() == Some(sym)
                    || handle.raw_syms().contains(&sym)
            }
            Key::Code(code) => handle.raw_code().raw() == code,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseBinding {
    pub mods: Modifiers,
    /// A linux input event code, like `BTN_LEFT`.
    pub button: u32,
}

impl MouseBinding {
    fn parse(s: &str) -> Result<Self, String> {
        let (mods, _, name) = split_binding(s)?;
        let button = match name {
            "left" => 0x110,
            "right" => 0x111,
            "middle" => 0x112,
            "side" => 0x113,
            "extra" => 0x114,
            "forward" => 0x115,
            "back" => 0x116,
            "task" => 0x117,
            _ => return Err(format!("unknown mouse button `{name}`")),
        };
        Ok(Self { mods, button })
    }
}

/// Splits `"LS+a"` into its modifiers, separator and key.
fn split_binding(s: &str) -> Result<(Modifiers, char, &str), String> {
    let index = s
        .find(['+', '-'])
        .ok_or_else(|| format!("binding `{s}` needs modifiers, like `A+{s}` or `0+{s}`"))?;
    let separator = s[index..].chars().next().unwrap();
    Ok((Modifiers::parse(&s[..index])?, separator, &s[index + 1..]))
}

fn parse_keysym(name: &str) -> Result<Keysym, String> {
    let mut sym = xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS);
    if sym == Keysym::NoSymbol {
        sym = xkb::keysym_from_name(name, xkb::KEYSYM_CASE_INSENSITIVE);
    }
    if sym == Keysym::NoSymbol {
        return Err(format!("unknown key `{name}`"));
    }
    // Bindings match unshifted keysyms, so `S+A` means the same as `S+a`.
    Ok(match sym.key_char() {
        Some(c) if c.is_uppercase() => Keysym::from_char(c.to_lowercase().next().unwrap()),
        _ => sym,
    })
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| e.to_string())?;

        let keyboard_bindings = raw
            .bindings
            .keyboard
            .iter()
            .map(|(key, action)| {
                let binding = KeyBinding::parse(key)?;
                Ok((
                    binding,
                    action
                        .parse::<Action>()
                        .map_err(|e| format!("{key}: {e}"))?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;

        let mouse_bindings = raw
            .bindings
            .mouse
            .iter()
            .map(|(key, action)| {
                let binding = MouseBinding::parse(key)?;
                Ok((
                    binding,
                    action
                        .parse::<Action>()
                        .map_err(|e| format!("{key}: {e}"))?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;

        let bound = keyboard_bindings
            .iter()
            .map(|(_, a)| a)
            .chain(mouse_bindings.iter().map(|(_, a)| a));
        for action in bound {
            if let Action::Command(name) = action {
                if !raw.actions.contains_key(name) {
                    return Err(format!(
                        "`action-{name}` is bound but not defined under [actions]"
                    ));
                }
            }
        }

        let mut layouts = HashMap::new();
        for (key, value) in &raw.layouts {
            let register = match key.chars().collect::<Vec<_>>()[..] {
                [c] if c.is_ascii_lowercase() || c.is_ascii_digit() => c,
                _ => return Err(format!("layout register `{key}` must be a-z or 0-9")),
            };
            layouts.insert(
                register,
                Layout::parse(value).map_err(|e| format!("layout {key}: {e}"))?,
            );
        }

        for mark in raw.marks.keys() {
            if !mark.is_ascii_lowercase() {
                return Err(format!("mark `{mark}` must be a-z"));
            }
        }

        Ok(Self {
            ui: raw.ui,
            actions: raw.actions,
            keyboard_bindings,
            mouse_bindings,
            layouts,
            marks: raw.marks,
            views: raw.views,
            keyboard: raw.inputs.keyboard,
        })
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!("{} not found, using the default config", path.display());
                Ok(Self::default_config())
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn default_config() -> Self {
        Self::parse(DEFAULT_CONFIG).expect("the default config is valid")
    }

    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_default();
        base.join("rose").join("config.toml")
    }

    pub fn key_action(
        &self,
        mods: &ModifiersState,
        handle: &KeysymHandle<'_>,
    ) -> Option<(KeyBinding, Action)> {
        self.keyboard_bindings
            .iter()
            .find(|(binding, _)| binding.matches(mods, handle))
            .cloned()
    }

    pub fn mouse_action(&self, mods: &ModifiersState, button: u32) -> Option<Action> {
        let binding = MouseBinding {
            mods: Modifiers::from_state(mods),
            button,
        };
        self.mouse_bindings
            .iter()
            .find(|(b, _)| *b == binding)
            .map(|(_, action)| action.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses() {
        Config::default_config();
    }

    #[test]
    fn bindings() {
        let b = KeyBinding::parse("LS+A").unwrap();
        assert_eq!(b.key, Key::Sym(Keysym::a));
        assert!(b.mods.logo && b.mods.shift && !b.mods.alt);
        assert_eq!(KeyBinding::parse("A-38").unwrap().key, Key::Code(38));
        assert_eq!(
            KeyBinding::parse("0+F1").unwrap().mods,
            Modifiers::default()
        );
        assert!(KeyBinding::parse("return").is_err());
        assert_eq!(MouseBinding::parse("A+left").unwrap().button, 0x110);
    }

    #[test]
    fn colors() {
        let c: Colorscheme = toml::from_str("active = 0xFF0000\ninactive = \"#00ff00\"").unwrap();
        assert_eq!(c.active.0, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(c.inactive.0, [0.0, 1.0, 0.0, 1.0]);
    }
}

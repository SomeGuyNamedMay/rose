//! Actions that bindings can trigger, named after hikari's actions.

use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cycle {
    Next,
    Prev,
    First,
    Last,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exchange {
    Next,
    Prev,
    Main,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetTarget {
    Number(u8),
    Alternate,
    Current,
}

/// A named position on the output, as in `view-move-top-left`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub horizontal: Align,
    pub vertical: Align,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Middle,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeKind {
    GroupAssign,
    InputGrab,
    MarkAssign,
    MarkSelect,
    MarkSwitchSelect,
    Move,
    Pan,
    Resize,
    SheetAssign,
}

/// What a menu lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuSource {
    /// Desktop entries.
    Applications,
    /// Executables in `PATH`.
    Run,
    /// Views, to switch to one.
    Views,
    /// Actions, to run one.
    Actions,
    /// Layout registers, to apply one.
    Layouts,
    /// The choice of whether to accept a wallpaper from an application.
    Wallpaper,
    /// A ranger-like file manager.
    Files,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    Reload,
    /// Runs the command configured under `[actions]` with this name.
    Command(String),

    GroupCycle(Cycle),
    GroupCycleView(Cycle),
    GroupHide,
    GroupLower,
    GroupRaise,
    GroupOnly,

    LayoutApply(char),
    LayoutCycleView(Cycle),
    LayoutExchangeView(Exchange),
    LayoutReset,
    LayoutRestack {
        append: bool,
    },

    MarkShow(char),
    MarkSwitchTo(char),

    EnterMode(ModeKind),
    Menu(MenuSource),

    SheetShowAll,
    SheetShowGroup,
    SheetShowInvisible,

    ViewCycle(Cycle),
    ViewResize {
        direction: Direction,
        grow: bool,
    },
    ViewMove(Direction),
    ViewMoveTo(Position),
    ViewHide,
    ViewLower,
    ViewRaise,
    ViewOnly,
    ViewPinToSheet(SheetTarget),
    ViewQuit,
    ViewResetGeometry,
    ViewSnap(Direction),
    ViewToggleFloating,
    ViewToggleInvisible,
    ViewToggleMaximize {
        horizontal: bool,
        vertical: bool,
    },

    WorkspaceClear,
    WorkspaceShowAll,
    WorkspaceShowGroup,
    WorkspaceShowInvisible,
    WorkspaceSwitchToSheet(SheetTarget),
    WorkspaceSwitchInhabited {
        next: bool,
    },
}

fn direction(s: &str) -> Option<Direction> {
    Some(match s {
        "up" => Direction::Up,
        "down" => Direction::Down,
        "left" => Direction::Left,
        "right" => Direction::Right,
        _ => return None,
    })
}

fn cycle(s: &str) -> Option<Cycle> {
    Some(match s {
        "next" => Cycle::Next,
        "prev" => Cycle::Prev,
        "first" => Cycle::First,
        "last" => Cycle::Last,
        _ => return None,
    })
}

fn sheet_target(s: &str) -> Option<SheetTarget> {
    Some(match s {
        "alternate" => SheetTarget::Alternate,
        "current" => SheetTarget::Current,
        _ => SheetTarget::Number(register(s)?.to_digit(10)? as u8),
    })
}

/// A single-character register name: `a`-`z` or `0`-`9`.
fn register(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let c = chars.next()?;
    (chars.next().is_none() && (c.is_ascii_lowercase() || c.is_ascii_digit())).then_some(c)
}

fn mark(s: &str) -> Option<char> {
    register(s).filter(char::is_ascii_lowercase)
}

fn position(s: &str) -> Option<Position> {
    use Align::*;
    let (horizontal, vertical) = match s {
        "center" => (Middle, Middle),
        "center-left" => (Start, Middle),
        "center-right" => (End, Middle),
        "top-left" => (Start, Start),
        "top-middle" => (Middle, Start),
        "top-right" => (End, Start),
        "bottom-left" => (Start, End),
        "bottom-middle" => (Middle, End),
        "bottom-right" => (End, End),
        _ => return None,
    };
    Some(Position {
        horizontal,
        vertical,
    })
}

fn mode(s: &str) -> Option<ModeKind> {
    Some(match s {
        "group-assign" => ModeKind::GroupAssign,
        "input-grab" => ModeKind::InputGrab,
        "mark-assign" => ModeKind::MarkAssign,
        "mark-select" => ModeKind::MarkSelect,
        "mark-switch-select" => ModeKind::MarkSwitchSelect,
        "move" => ModeKind::Move,
        "pan" => ModeKind::Pan,
        "resize" => ModeKind::Resize,
        "sheet-assign" => ModeKind::SheetAssign,
        _ => return None,
    })
}

/// Actions whose names take no argument.
fn fixed_actions() -> Vec<(&'static str, Action)> {
    use Action::*;
    vec![
        ("quit", Quit),
        ("reload", Reload),
        ("group-cycle-next", GroupCycle(Cycle::Next)),
        ("group-cycle-prev", GroupCycle(Cycle::Prev)),
        ("group-hide", GroupHide),
        ("group-lower", GroupLower),
        ("group-raise", GroupRaise),
        ("group-only", GroupOnly),
        (
            "layout-exchange-view-next",
            LayoutExchangeView(Exchange::Next),
        ),
        (
            "layout-exchange-view-prev",
            LayoutExchangeView(Exchange::Prev),
        ),
        (
            "layout-exchange-view-main",
            LayoutExchangeView(Exchange::Main),
        ),
        ("layout-reset", LayoutReset),
        ("layout-restack-append", LayoutRestack { append: true }),
        ("layout-restack-prepend", LayoutRestack { append: false }),
        ("sheet-show-all", SheetShowAll),
        ("sheet-show-group", SheetShowGroup),
        ("sheet-show-invisible", SheetShowInvisible),
        ("view-cycle-next", ViewCycle(Cycle::Next)),
        ("view-cycle-prev", ViewCycle(Cycle::Prev)),
        ("view-hide", ViewHide),
        ("view-lower", ViewLower),
        ("view-raise", ViewRaise),
        ("view-only", ViewOnly),
        ("view-quit", ViewQuit),
        ("view-reset-geometry", ViewResetGeometry),
        ("view-toggle-floating", ViewToggleFloating),
        ("view-toggle-invisible", ViewToggleInvisible),
        (
            "view-toggle-maximize-full",
            ViewToggleMaximize {
                horizontal: true,
                vertical: true,
            },
        ),
        (
            "view-toggle-maximize-horizontal",
            ViewToggleMaximize {
                horizontal: true,
                vertical: false,
            },
        ),
        (
            "view-toggle-maximize-vertical",
            ViewToggleMaximize {
                horizontal: false,
                vertical: true,
            },
        ),
        ("workspace-clear", WorkspaceClear),
        ("workspace-show-all", WorkspaceShowAll),
        ("workspace-show-group", WorkspaceShowGroup),
        ("workspace-show-invisible", WorkspaceShowInvisible),
        (
            "workspace-switch-to-sheet-next-inhabited",
            WorkspaceSwitchInhabited { next: true },
        ),
        (
            "workspace-switch-to-sheet-prev-inhabited",
            WorkspaceSwitchInhabited { next: false },
        ),
    ]
}

/// The name of every action, for the action menu. Commands, marks and layout registers are left
/// out; the menu adds the ones that are configured.
pub fn all_names() -> Vec<String> {
    let mut names: Vec<String> = fixed_actions()
        .into_iter()
        .map(|(name, _)| name.to_string())
        .collect();
    let with = |prefix: &str, args: &[&str]| -> Vec<String> {
        args.iter().map(|arg| format!("{prefix}{arg}")).collect()
    };
    let sheets = [
        "0",
        "1",
        "2",
        "3",
        "4",
        "5",
        "6",
        "7",
        "8",
        "9",
        "alternate",
    ];
    let directions = ["up", "down", "left", "right"];
    let cycles = ["next", "prev", "first", "last"];
    names.extend(with("group-cycle-view-", &cycles));
    names.extend(with("layout-cycle-view-", &cycles));
    names.extend(with(
        "menu-",
        &[
            "applications",
            "run",
            "views",
            "actions",
            "layouts",
            "files",
        ],
    ));
    names.extend(with(
        "mode-enter-",
        &[
            "group-assign",
            "mark-assign",
            "mark-select",
            "mark-switch-select",
            "move",
            "pan",
            "resize",
            "sheet-assign",
        ],
    ));
    names.extend(with("view-increase-size-", &directions));
    names.extend(with("view-decrease-size-", &directions));
    names.extend(with("view-move-", &directions));
    names.extend(with(
        "view-move-",
        &[
            "center",
            "center-left",
            "center-right",
            "top-left",
            "top-middle",
            "top-right",
            "bottom-left",
            "bottom-middle",
            "bottom-right",
        ],
    ));
    names.extend(with("view-pin-to-sheet-", &sheets));
    names.extend(with("view-snap-", &directions));
    names.extend(with("workspace-switch-to-sheet-", &sheets));
    names.sort();
    names
}

fn menu_source(s: &str) -> Option<MenuSource> {
    Some(match s {
        "applications" => MenuSource::Applications,
        "run" => MenuSource::Run,
        "views" => MenuSource::Views,
        "actions" => MenuSource::Actions,
        "layouts" => MenuSource::Layouts,
        "files" => MenuSource::Files,
        _ => return None,
    })
}

impl FromStr for Action {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use Action::*;

        if let Some((_, action)) = fixed_actions().into_iter().find(|(name, _)| *name == s) {
            return Ok(action);
        }

        let parsed = if let Some(name) = s.strip_prefix("action-") {
            Some(Command(name.to_string()))
        } else if let Some(rest) = s.strip_prefix("group-cycle-view-") {
            cycle(rest).map(GroupCycleView)
        } else if let Some(rest) = s.strip_prefix("layout-apply-") {
            register(rest)
                .filter(char::is_ascii_lowercase)
                .map(LayoutApply)
        } else if let Some(rest) = s.strip_prefix("layout-cycle-view-") {
            cycle(rest).map(LayoutCycleView)
        } else if let Some(rest) = s.strip_prefix("mark-show-") {
            mark(rest).map(MarkShow)
        } else if let Some(rest) = s.strip_prefix("mark-switch-to-") {
            mark(rest).map(MarkSwitchTo)
        } else if let Some(rest) = s.strip_prefix("menu-") {
            menu_source(rest).map(Menu)
        } else if let Some(rest) = s.strip_prefix("mode-enter-") {
            mode(rest).map(EnterMode)
        } else if let Some(rest) = s.strip_prefix("view-increase-size-") {
            direction(rest).map(|direction| ViewResize {
                direction,
                grow: true,
            })
        } else if let Some(rest) = s.strip_prefix("view-decrease-size-") {
            direction(rest).map(|direction| ViewResize {
                direction,
                grow: false,
            })
        } else if let Some(rest) = s.strip_prefix("view-move-") {
            direction(rest)
                .map(ViewMove)
                .or_else(|| position(rest).map(ViewMoveTo))
        } else if let Some(rest) = s.strip_prefix("view-pin-to-sheet-") {
            sheet_target(rest).map(ViewPinToSheet)
        } else if let Some(rest) = s.strip_prefix("view-snap-") {
            direction(rest).map(ViewSnap)
        } else if let Some(rest) = s.strip_prefix("workspace-switch-to-sheet-") {
            sheet_target(rest).map(WorkspaceSwitchToSheet)
        } else {
            None
        };

        parsed.ok_or_else(|| format!("unknown action `{s}`"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_parses() {
        for name in all_names() {
            assert!(name.parse::<Action>().is_ok(), "{name}");
        }
        assert_eq!(
            "menu-run".parse::<Action>(),
            Ok(Action::Menu(MenuSource::Run))
        );
    }
}

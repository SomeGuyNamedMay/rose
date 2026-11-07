//! End-to-end tests: Rose runs nested in a headless sway, gets real keyboard and pointer input,
//! and the tests check what ends up on screen.
//!
//! They need `sway` and `grim` on PATH, so they're ignored by default:
//!
//!     nix develop -c cargo test --test e2e -- --ignored
//!
//! Geometry below assumes the test config: a 1200x800 output, gap 12 and border 4. Floating test
//! windows are 400x300.

mod client;
mod harness;
mod input;

use harness::{Session, ACTIVE, BACKGROUND, CONFLICT, GROUPED, INACTIVE, INSERT, OUTPUT_HEIGHT};
use input::Button;

const RED: [u8; 3] = [0xFF, 0x00, 0x00];
const GREEN: [u8; 3] = [0x00, 0xFF, 0x00];
const BLUE: [u8; 3] = [0x00, 0x00, 0xFF];

/// Places the test windows apart from each other. Centers: red (250, 200), green (850, 200),
/// blue (550, 570). Their left borders are 2px left of x = 50, 650 and 350.
const FLOATING: &str = r#"
[views.red]
position = { x = 50, y = 50 }
[views.green]
position = { x = 650, y = 50 }
[views.blue]
position = { x = 350, y = 420 }
"#;

const RED_CENTER: (u32, u32) = (250, 200);
const GREEN_CENTER: (u32, u32) = (850, 200);
const BLUE_CENTER: (u32, u32) = (550, 570);

// Layout `s` on the test output: the main tile's content spans (16, 16) to (709, 784); the stack
// tiles' content starts at x = 729, with y from 16 to 390 and from 410 to 784.
const MAIN_CENTER: (u32, u32) = (362, 400);
const STACK_TOP_CENTER: (u32, u32) = (956, 203);
const STACK_BOTTOM_CENTER: (u32, u32) = (956, 597);
const MAIN_BORDER: (u32, u32) = (13, 400);
const STACK_TOP_BORDER: (u32, u32) = (726, 200);
const STACK_BOTTOM_BORDER: (u32, u32) = (726, 600);

/// Opens red, green and blue in that order (so blue is on top and focused) and tiles them with
/// layout `s`, which puts the topmost view in the main tile.
fn tiled_session() -> (Session, Vec<client::Client>) {
    let mut s = Session::start("");
    let clients = vec![
        s.client("red", RED, (200, 150)),
        s.client("green", GREEN, (200, 150)),
        s.client("blue", BLUE, (200, 150)),
    ];
    s.input.key(&["alt", "l"]);
    s.input.key(&["s"]);
    s.input.key(&["return"]);
    (s, clients)
}

fn floating_session() -> (Session, Vec<client::Client>) {
    let s = Session::start(FLOATING);
    let clients = vec![
        s.client("red", RED, RED_CENTER),
        s.client("green", GREEN, GREEN_CENTER),
        s.client("blue", BLUE, BLUE_CENTER),
    ];
    (s, clients)
}

#[track_caller]
fn at(s: &Session, point: (u32, u32), color: [u8; 3]) {
    s.wait_pixel(point.0, point.1, color);
}

#[test]
#[ignore = "needs sway and grim; run with `cargo test --test e2e -- --ignored`"]
fn layout_tiles_views_topmost_first() {
    let (s, _clients) = tiled_session();
    at(&s, MAIN_CENTER, BLUE);
    at(&s, STACK_TOP_CENTER, GREEN);
    at(&s, STACK_BOTTOM_CENTER, RED);
    at(&s, MAIN_BORDER, ACTIVE);
    at(&s, STACK_TOP_BORDER, INACTIVE);
    // The gap between tiles shows the background.
    at(&s, (717, 400), BACKGROUND);
}

#[test]
#[ignore = "needs sway and grim; run with `cargo test --test e2e -- --ignored`"]
fn cycle_maximize_and_exchange_with_main() {
    let (mut s, _clients) = tiled_session();
    at(&s, MAIN_CENTER, BLUE);

    // Cycling goes in creation order and wraps from blue to red.
    s.input.key(&["alt", "j"]);
    at(&s, STACK_BOTTOM_BORDER, ACTIVE);
    at(&s, MAIN_BORDER, INACTIVE);

    s.input.key(&["alt", "f"]);
    at(&s, (600, 400), RED);
    at(&s, (13, 13), ACTIVE);

    // Unmaximizing returns the view to its tile.
    s.input.key(&["alt", "f"]);
    at(&s, STACK_BOTTOM_CENTER, RED);
    at(&s, MAIN_CENTER, BLUE);

    s.input.key(&["alt", "shift", "return"]);
    at(&s, MAIN_CENTER, RED);
    at(&s, STACK_BOTTOM_CENTER, BLUE);
}

#[test]
#[ignore = "needs sway and grim; run with `cargo test --test e2e -- --ignored`"]
fn sheets_marks_and_borrowing() {
    let (mut s, _clients) = floating_session();

    // Mark blue, then move it to sheet 2.
    s.input.key(&["alt", "m"]);
    s.input.key(&["a"]);
    s.input.key(&["return"]);
    s.input.key(&["alt", "shift", "2"]);
    at(&s, BLUE_CENTER, BACKGROUND);
    at(&s, RED_CENTER, RED);

    s.input.key(&["alt", "2"]);
    at(&s, BLUE_CENTER, BLUE);
    at(&s, RED_CENTER, BACKGROUND);
    at(&s, GREEN_CENTER, BACKGROUND);

    s.input.key(&["alt", "backspace"]);
    at(&s, RED_CENTER, RED);
    at(&s, BLUE_CENTER, BACKGROUND);

    // Showing the mark borrows blue into sheet 1; switching to the current sheet resets it.
    s.input.key(&["alt", "apostrophe"]);
    s.input.key(&["a"]);
    at(&s, BLUE_CENTER, BLUE);
    s.input.key(&["alt", "1"]);
    at(&s, BLUE_CENTER, BACKGROUND);
    at(&s, RED_CENTER, RED);

    // Views on sheet 0 stay visible on every sheet.
    s.input.click(GREEN_CENTER.0, GREEN_CENTER.1);
    s.input.key(&["alt", "shift", "0"]);
    s.input.key(&["alt", "2"]);
    at(&s, GREEN_CENTER, GREEN);
    at(&s, BLUE_CENTER, BLUE);
    at(&s, RED_CENTER, BACKGROUND);

    // Mark-switch-select switches to the sheet of the marked view.
    s.input.key(&["alt", "1"]);
    at(&s, RED_CENTER, RED);
    s.input.key(&["alt", "shift", "apostrophe"]);
    s.input.key(&["a"]);
    at(&s, RED_CENTER, BACKGROUND);
    at(&s, BLUE_CENTER, BLUE);
}

#[test]
#[ignore = "needs sway and grim; run with `cargo test --test e2e -- --ignored`"]
fn group_and_mark_modes_show_in_border_colors() {
    let (mut s, _clients) = floating_session();
    let red_border = (48, 200);
    let green_border = (648, 200);

    // Typing a group name for blue shows the insert color until it's confirmed.
    s.input.key(&["alt", "g"]);
    s.input.type_text("we");
    at(&s, (348, 570), INSERT);
    s.input.type_text("b");
    s.input.key(&["return"]);
    at(&s, (348, 570), ACTIVE);

    // Once green joins blue's group, it's drawn grouped while blue is focused.
    s.input.click(GREEN_CENTER.0, GREEN_CENTER.1);
    s.input.key(&["alt", "g"]);
    s.input.type_text("web");
    s.input.key(&["return"]);
    s.input.click(BLUE_CENTER.0, BLUE_CENTER.1);
    at(&s, green_border, GROUPED);
    at(&s, red_border, INACTIVE);

    // Typing a mark that belongs to another view shows the conflict color.
    s.input.key(&["alt", "m"]);
    s.input.key(&["a"]);
    s.input.key(&["return"]);
    s.input.click(RED_CENTER.0, RED_CENTER.1);
    s.input.key(&["alt", "m"]);
    s.input.key(&["a"]);
    at(&s, red_border, CONFLICT);
    s.input.key(&["escape"]);
    at(&s, red_border, ACTIVE);
}

#[test]
#[ignore = "needs sway and grim; run with `cargo test --test e2e -- --ignored`"]
fn mouse_bindings_move_and_resize() {
    let mut s = Session::start(FLOATING);
    let _red = s.client("red", RED, RED_CENTER);

    s.input.hold("alt");
    s.input.drag(Button::Left, (250, 200), (450, 400));
    s.input.release("alt");
    // Red now spans (250, 250) to (650, 550).
    at(&s, (60, 60), BACKGROUND);
    at(&s, (450, 400), RED);
    at(&s, (247, 400), ACTIVE);

    // Grabbing near the bottom-right corner resizes from there.
    s.input.hold("alt");
    s.input.drag(Button::Right, (600, 500), (700, 600));
    s.input.release("alt");
    at(&s, (740, 640), RED);
    at(&s, (260, 260), RED);
}

#[test]
#[ignore = "needs sway and grim; run with `cargo test --test e2e -- --ignored`"]
fn client_requested_move_and_resize() {
    let mut s = Session::start(FLOATING);
    let _red = s.client("red", RED, RED_CENTER);

    // The test client asks to be moved on a left click.
    s.input.drag(Button::Left, (250, 200), (350, 300));
    at(&s, (60, 60), BACKGROUND);
    at(&s, (540, 440), RED);

    // And to be resized from its top-left corner on a right click; the bottom-right corner must
    // stay where it is. Red spans (150, 150) to (550, 450) before this.
    s.input.drag(Button::Right, (300, 300), (200, 200));
    at(&s, (60, 60), RED);
    at(&s, (545, 445), RED);
    at(&s, (558, 458), BACKGROUND);
}

#[test]
#[ignore = "needs sway and grim; run with `cargo test --test e2e -- --ignored`"]
fn snap_and_keyboard_resize() {
    let mut s = Session::start(FLOATING);
    let _red = s.client("red", RED, RED_CENTER);

    // Snapping right leaves the gap and the border at the output edge: content ends at x = 1184.
    s.input.key(&["alt", "ctrl", "l"]);
    at(&s, (1182, 200), RED);
    at(&s, (1186, 200), ACTIVE);
    at(&s, (1194, 200), BACKGROUND);

    // Holding the resize binding resizes from the bottom-right corner as the pointer moves.
    s.input.move_to(600, 600);
    s.input.hold("alt");
    s.input.hold("shift");
    s.input.hold("e");
    s.input.move_to(700, 650);
    s.input.release("e");
    s.input.release("shift");
    s.input.release("alt");
    at(&s, (800, 395), RED);
    at(&s, (800, 402), ACTIVE);
}

#[test]
#[ignore = "needs sway and grim; run with `cargo test --test e2e -- --ignored`"]
fn input_grab_sends_bindings_to_the_view() {
    let (mut s, _clients) = floating_session();
    let blue_border = (348, 570);

    s.input.key(&["alt", "shift", "i"]);
    at(&s, blue_border, INSERT);

    // Alt+J would cycle focus, but the grab sends it to the view instead.
    s.input.key(&["alt", "j"]);
    at(&s, blue_border, INSERT);

    s.input.key(&["alt", "shift", "i"]);
    at(&s, blue_border, ACTIVE);
    s.input.key(&["alt", "j"]);
    at(&s, blue_border, INACTIVE);
}

#[test]
#[ignore = "needs sway and grim; run with `cargo test --test e2e -- --ignored`"]
fn reload_applies_a_changed_config() {
    let (mut s, _clients) = floating_session();
    at(&s, (348, 570), ACTIVE);

    let cyan = [0x00, 0xFF, 0xFF];
    let config = std::fs::read_to_string(&s.config_path).unwrap();
    s.write_config(&config.replace("active = 0xFFFFFF", "active = 0x00FFFF"));
    s.input.key(&["alt", "ctrl", "r"]);
    at(&s, (348, 570), cyan);
}

#[test]
#[ignore = "needs sway and grim; run with `cargo test --test e2e -- --ignored`"]
fn quit_closes_views_then_exits() {
    let (mut s, clients) = floating_session();
    s.input.key(&["alt", "ctrl", "q"]);
    assert!(
        s.wait_for_exit(),
        "rose did not exit after its views closed"
    );
    // Each client records that it closed right after telling rose.
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        clients.iter().all(|c| c.is_closed()),
        "not every view was asked to close"
    );
}

/// The default theme's background, behind the bar and the menu's items.
const BAR_BACKGROUND: [u8; 3] = [0x1E, 0x21, 0x27];
/// The default theme's `secondary`, behind the current sheet's number.
const CURRENT_SHEET: [u8; 3] = [0x5C, 0x63, 0x70];

#[test]
#[ignore = "needs sway and grim; run with `cargo test --test e2e -- --ignored`"]
fn bar_reserves_space_and_switches_sheets() {
    let mut s = Session::start("[bar]\n");
    let _red = s.client("red", RED, (200, 150));
    s.wait_pixel(600, OUTPUT_HEIGHT - 2, BAR_BACKGROUND);

    // The bar's height depends on the font, so find its top edge.
    let shot = s.screenshot();
    let top = (0..OUTPUT_HEIGHT)
        .rev()
        .find(|&y| shot.pixel(600, y) != BAR_BACKGROUND)
        .unwrap()
        + 1;
    assert!(
        top < OUTPUT_HEIGHT - 10,
        "the bar is only {} px high",
        OUTPUT_HEIGHT - top
    );

    // Maximized views stop a gap and a border above the bar.
    s.input.key(&["alt", "f"]);
    at(&s, (600, top - 17), RED);
    at(&s, (600, top - 16), ACTIVE);
    at(&s, (600, top - 1), BACKGROUND);

    // Clicking a sheet in the bar switches to it. Sheet 2 comes right after the current sheet 1.
    s.input.key(&["alt", "shift", "2"]);
    at(&s, (600, 400), BACKGROUND);
    let shot = s.screenshot();
    let y = top + 2;
    let start = (0..600)
        .find(|&x| shot.pixel(x, y) == CURRENT_SHEET)
        .unwrap();
    let end = (start..600)
        .find(|&x| shot.pixel(x, y) != CURRENT_SHEET)
        .unwrap();
    s.input.click(end + (end - start) / 2, y);
    at(&s, (600, 400), RED);
}

#[test]
#[ignore = "needs sway and grim; run with `cargo test --test e2e -- --ignored`"]
fn menu_switches_views_and_runs_actions() {
    let (mut s, _clients) = floating_session();
    // The menu opens at the bottom of the output, covering this point.
    let menu = (600, 790);

    // Choosing a view on another sheet switches to it.
    s.input.key(&["alt", "shift", "2"]);
    at(&s, BLUE_CENTER, BACKGROUND);
    s.input.key(&["alt", "slash"]);
    at(&s, menu, BAR_BACKGROUND);
    s.input.type_text("blue");
    s.input.key(&["return"]);
    at(&s, BLUE_CENTER, BLUE);
    at(&s, RED_CENTER, BACKGROUND);
    at(&s, menu, BACKGROUND);

    s.input.key(&["alt", "semicolon"]);
    s.input.type_text("maximize full");
    s.input.key(&["return"]);
    at(&s, (600, 300), BLUE);

    // Escape and clicking outside close the menu without doing anything.
    s.input.key(&["alt", "semicolon"]);
    at(&s, (600, 700), BAR_BACKGROUND);
    s.input.type_text("quit");
    s.input.key(&["escape"]);
    at(&s, (600, 700), BLUE);
    s.input.key(&["alt", "slash"]);
    at(&s, (600, 700), BAR_BACKGROUND);
    s.input.click(600, 100);
    at(&s, (600, 700), BLUE);
}

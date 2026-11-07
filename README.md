# rose

A Wayland compositor built on [Smithay](https://github.com/Smithay/smithay) 0.7, with window
management modeled on [hikari](https://hikari.acmelabs.space/): sheets, groups, marks,
keyboard-driven modes and on-demand tiling layouts.

Rose runs nested in a window inside your existing Wayland or X11 session (the winit backend).

## Running

```sh
nix run                                  # starts rose and opens the configured terminal (foot)
nix run . -- -c weston-terminal          # start a different client
nix run . -- --config ./my-config.toml   # use a specific config file
```

Other clients can connect using the socket rose logs on startup (`WAYLAND_DISPLAY=wayland-1 some-app`).
Set `RUST_LOG=debug` for more logging.

## Configuration

Rose reads `$XDG_CONFIG_HOME/rose/config.toml` (usually `~/.config/rose/config.toml`) and falls back
to the built-in [`config.toml`](config.toml), which documents every option. Copy it as a starting
point. The `reload` action (Alt+Ctrl+R) re-reads it without restarting.

The default bindings use **Alt** instead of hikari's logo key, because desktops like GNOME reserve
most logo-key shortcuts while Rose runs nested inside them. Some highlights:

| Keys | Action |
| --- | --- |
| Alt+Return | open a terminal |
| Alt+1 … Alt+0, Alt+BackSpace | switch to a sheet, or to the alternate sheet |
| Alt+Shift+1 … Alt+Shift+0 | move the focused view to a sheet |
| Alt+J / Alt+K | cycle views |
| Alt+L | pick a layout from the menu (`s`, `q`, `f`, `g`, `m` by default) |
| Alt+A / Alt+Shift+L | add views to the layout / reset the layout |
| Alt+F | toggle maximize |
| Alt+G, type a name, Return | assign the focused view to a group |
| Alt+M, a letter, Return | mark the focused view |
| Alt+' then a letter | show the marked view |
| Alt+D / Alt+; | open the application / action menu |
| Alt+drag / Alt+right-drag | move / resize |
| Alt+Shift+Q / Alt+Ctrl+Q | close the view / quit |

## Window management

- **Sheets 1-9** hold views; switching sheets replaces what's shown. Views on **sheet 0** are always
  visible, below the others. Switching to the current sheet resets it.
- **Groups** are named sets of views that can span sheets. A view's group defaults to its app id.
  The focused view's border is `active`; other views in its group use `grouped`.
- **Marks** `a`-`z` jump to a view, borrowing it into the current sheet. Unbound marks can run a
  command from `[marks]`.
- **Layouts** are binary split trees of containers (`single`, `stack`, `queue`, `grid`, `full`,
  `empty`) stored in registers. They tile a sheet on demand; new views float until restacked.
  `layout-exchange-view-*` and `layout-cycle-view-*` work on the layout order.
- **Modes**: group-assign, mark-assign, mark-select, mark-switch-select, sheet-assign,
  input-grab (all keys go to the focused view until the binding is pressed again), and keyboard
  move/resize (hold the binding and move the pointer; releasing a key ends it). While a mode waits
  for input, the focused view's border uses the `insert` color, or `conflict` for a taken mark.
- **Views** can be floating, invisible, maximized horizontally/vertically/fully, moved and resized by
  `step`, moved to named positions, snapped to edges and neighbors, hidden, and reset to their
  original geometry.
- `quit` asks every view to close; Rose exits once they have, or immediately on a second `quit`.

All of hikari's actions are available except those that need hardware or multiple outputs
(`lock`, `vt-switch-to-*`, `workspace-cycle-*`, `view-toggle-public`).

Rose also supports server-side decorations (`xdg-decoration`), client cursor images, clipboard and
drag-and-drop, and client-initiated move, resize and maximize.

## Bar and menu

Rose has a built-in bar styled after [Nyxt](https://nyxt-browser.com/)'s status buffer: slanted
segments for the sheets in use, the focused view's title and the current mode, with the other visible
views listed like tabs and a status line from `status-command`. Click a sheet or a view to switch to
it. While a mode waits for input, the bar shows what has been typed in the `insert` color.

The menu is a launcher like rofi, styled after Nyxt's prompt buffer: a prompt with the typed input
and match count, a heading for the source, and a table of suggestions narrowed down by fuzzy
matching. It opens next to the bar:

| Keys | Menu |
| --- | --- |
| Alt+D | `menu-applications`: desktop entries |
| Alt+Shift+D | `menu-run`: programs in `PATH`, or any typed command (Shift+Return) |
| Alt+/ | `menu-views`: switch to a view on any sheet |
| Alt+; | `menu-actions`: run any action, showing its binding |

Up/Down, Tab or Ctrl+N/P select, Return or a click accepts, and Escape, Ctrl+G or a click elsewhere
closes it. Fonts and colors are under `[bar]` in the config.

## Differences from hikari

- Instead of indicator bars on each view, one bar shows the focused view and what modes have typed.
- Single output, nested only; no DRM/libinput backend, layer-shell, XWayland or screen locking.
- Keyboard move/resize can't warp the host pointer, so they follow pointer movement from wherever it is.
- Configuration is TOML rather than UCL, with the same structure where it applies.
- New views are placed centered on the pointer; their group defaults to their app id.

## Developing

```sh
nix develop
cargo run      # same as `nix run`, from source
cargo test     # unit tests
```

### End-to-end tests

`tests/e2e` runs Rose nested inside a headless [sway](https://swaywm.org/), sends it real keyboard and
mouse input through sway's virtual input protocols, opens test windows that fill themselves with a
solid color, and checks pixels in screenshots taken with `grim`. They cover tiling, cycling,
maximizing, sheets, marks, groups, modes, mouse and keyboard move/resize, client-requested
move/resize, snapping, input grabs, reload, quit, and the bar and menu.

They need sway and grim, so plain `cargo test` skips them. Run them with:

```sh
nix develop -c cargo test --test e2e -- --ignored   # uses your GPU, ~15s
nix flake check                                     # in the Nix sandbox with software rendering
```

When a test fails it names the pixel that was wrong and keeps a directory with the screenshot
(`failure.ppm`) and the sway and Rose logs.

# rose

A Wayland compositor built on [Smithay](https://github.com/Smithay/smithay) 0.7, with window
management modeled on [hikari](https://hikari.acmelabs.space/): sheets, groups, marks,
keyboard-driven modes and on-demand tiling layouts.

Rose runs either as your login session, directly on the hardware, or nested in a window inside an
existing Wayland or X11 session.

## Running

```sh
nix run                                  # starts rose and opens the configured terminal (foot)
nix run . -- -c weston-terminal          # start a different client
nix run . -- --config ./my-config.toml   # use a specific config file
```

Other clients can connect using the socket rose logs on startup (`WAYLAND_DISPLAY=wayland-1 some-app`).
Set `RUST_LOG=debug` for more logging.

Rose nests when `WAYLAND_DISPLAY` or `DISPLAY` is set and otherwise takes over the screen, using
logind or seatd for access to it. `--nested` and `--tty` override that. To run it without a display
manager, log in on a VT and run `rose`. Ctrl+Alt+F1 to F12 switch VTs.

## Configuration

Rose reads `$XDG_CONFIG_HOME/rose/config.toml` (usually `~/.config/rose/config.toml`) and falls back
to the built-in [`config.toml`](config.toml), which documents every option. Copy it as a starting
point. The `reload` action (Alt+Ctrl+R) re-reads it without restarting.

To set a wallpaper, point `[ui.wallpaper]` at a still image (PNG, JPEG, WebP or GIF) or an animated
GIF, PNG or WebP:

```toml
[ui.wallpaper]
path = "~/Pictures/wallpaper.gif"
mode = "fill"  # or "fit", "stretch", "center"
```

Rose is also a backend for the [wallpaper portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Wallpaper.html),
so applications can offer "Set as wallpaper". When an application asks for a preview, Rose asks you
in the menu first. The image is copied to `$XDG_STATE_HOME/rose` and survives restarts, until you
change `path` in the config.

The portal needs setting up outside Rose; see [Portal routing](#portal-routing).

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
| Alt+M, then h/j/k/l, Escape | move around the infinite desktop |
| Alt+Ctrl+M, a letter, Return | mark the focused view |
| Alt+' then a letter | show the marked view |
| Alt+D / Alt+; | open the application / action menu |
| Alt+drag / Alt+right-drag | move / resize |
| Alt+Shift+Q / Alt+Ctrl+Q | close the view / quit |

## NixOS and Home Manager

The flake provides two modules:

- `nixosModules.default` installs Rose system-wide and adds it to the display manager's session list.
- `homeManagerModules.default` installs Rose for one user and writes their `config.toml`.

You can use either or both. A typical setup uses the NixOS module for the login session and Home
Manager for the configuration:

```nix
# flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rose = {
      url = "github:SomeGuyNamedMay/rose"; # or "path:/home/you/rose" for a local checkout
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, home-manager, rose, ... }:
    {
      nixosConfigurations.my-machine = nixpkgs.lib.nixosSystem {
        modules = [
          ./configuration.nix

          rose.nixosModules.default
          { programs.rose.enable = true; }

          home-manager.nixosModules.home-manager
          {
            home-manager.users.you = {
              imports = [ rose.homeManagerModules.default ];
              programs.rose = {
                enable = true;
                settings.ui.gap = 8;
              };
              home.stateVersion = "25.05";
            };
          }
        ];
      };
    };
}
```

Keep `inputs.nixpkgs.follows`: Rose loads the system's graphics drivers, and on the hardware they
need to come from the same nixpkgs as Rose.

### NixOS module

Import `rose.nixosModules.default`, then:

```nix
{
  programs.rose.enable = true;
}
```

After `nixos-rebuild switch`, pick **Rose** in your display manager's session menu. Your display
manager still has to be enabled separately, for example with `services.displayManager.gdm.enable`.
To log in to Rose by default, also set `services.displayManager.defaultSession = "rose";`.

| Option | Default | Description |
| --- | --- | --- |
| `programs.rose.enable` | `false` | Install Rose. |
| `programs.rose.package` | this flake's package | The package to use. |
| `programs.rose.session.enable` | `true` | List Rose in display managers, and enable what a desktop session needs: graphics drivers, polkit, default fonts and audio. |
| `programs.rose.portal.enable` | `true` | Register the wallpaper portal backend, turning on `xdg.portal`. |

To only run Rose nested, set `programs.rose.session.enable = false;`.

### Home Manager module

With Home Manager running standalone, add the module to your configuration:

```nix
homeConfigurations.you = home-manager.lib.homeManagerConfiguration {
  pkgs = nixpkgs.legacyPackages.x86_64-linux;
  modules = [
    rose.homeManagerModules.default
    ./home.nix
  ];
};
```

Then configure it, here or in the NixOS example above:

```nix
{
  programs.rose = {
    enable = true;
    settings = {
      ui.gap = 8;
      ui.wallpaper.path = "~/Pictures/wallpaper.png";
      bar.font-size = 16;
      bindings.keyboard."AS+Return" = "action-terminal";
    };
  };
}
```

`settings` takes the same keys as [`config.toml`](config.toml), which documents all of them. Key
bindings are written `"<modifiers>+<keysym>"`, with `A` for Alt, `S` for Shift, `C` for Control and
`L` for the logo key.

| Option | Default | Description |
| --- | --- | --- |
| `programs.rose.enable` | `false` | Install Rose. |
| `programs.rose.package` | this flake's package | The package to use. |
| `programs.rose.settings` | `{ }` | The contents of `config.toml`. |
| `programs.rose.mergeDefaults` | `true` | Merge `settings` over the built-in config. |
| `programs.rose.session.enable` | `true` | Add the `rose-session` systemd target. |
| `programs.rose.portal.enable` | `false` | Register the portal backend through Home Manager's `xdg.portal`. |

A config file normally replaces the built-in config, including every key binding, so
`mergeDefaults` keeps the defaults for anything `settings` doesn't mention. To remove a default
binding, turn it off and write the whole config in `settings`. With `settings` empty and
`mergeDefaults` on, no file is written, which leaves `~/.config/rose/config.toml` yours to manage.

The options aren't validated at build time. If `settings` has a mistake, Rose logs an error and
starts with its built-in config, so check its log if your changes seem to be ignored. On a login
session, find it with `journalctl -b | grep rose`.

Home Manager can't add Rose to a display manager's session list, because that's system
configuration. Without the NixOS module, start `rose` from a VT. When Rose runs on the hardware it
starts `rose-session.target`, which also starts `graphical-session.target`, so user services that
are tied to the graphical session run with it.

The generated config is read-only, and the `reload` action picks up changes after
`home-manager switch`. A wallpaper set through the portal is kept in `~/.local/state/rose`, and stays
until `ui.wallpaper.path` in the config changes.

Enable the portal in only one of the two modules. On NixOS that's the NixOS module, which is on by
default. Home Manager's `portal.enable` is for other distributions.

### Portal routing

`xdg-desktop-portal` picks backends by `XDG_CURRENT_DESKTOP`. The package installs `rose.portal` and
`rose-portals.conf`, which send wallpaper requests to Rose when that variable is `rose`, as it is
in Rose's login session.

Nested in another desktop, that desktop's portal config decides, so nothing reaches Rose until you
route it there. On GNOME with NixOS:

```nix
{
  xdg.portal.config.gnome = {
    default = [ "gnome" "gtk" ];
    "org.freedesktop.impl.portal.Access" = [ "gtk" ];
    "org.freedesktop.impl.portal.Notification" = [ "gtk" ];
    "org.freedesktop.impl.portal.Secret" = [ "gnome-keyring" ];
    "org.freedesktop.impl.portal.Wallpaper" = [ "rose" ];
  };
}
```

Without NixOS, put the same settings in `~/.config/xdg-desktop-portal/gnome-portals.conf`. This
replaces GNOME's own portal config, so it repeats GNOME's defaults. Log out and back in afterwards.
With this routing, every application's wallpaper request in that desktop goes to Rose, and fails
while Rose isn't running.

## Window management

- **The desktop is an infinite plane**, shared by all sheets, and the output shows part of it.
  `mode-enter-pan` (Alt+M) lets h/j/k/l move the view by `step`, for as long as they are held, until
  Escape. The bar shows the position. Going to a view that is out of sight, by cycling, a mark, the
  bar or a menu, moves the view of the plane to it. The wallpaper and bar stay put.
- **Sheets 1-9** hold views; switching sheets replaces what's shown. Views on **sheet 0** are always
  visible, below the others. Switching to the current sheet resets it.
- **Groups** are named sets of views that can span sheets. A view's group defaults to its app id.
  The focused view's border is `active`; other views in its group use `grouped`.
- **Marks** `a`-`z` jump to a view, borrowing it into the current sheet. Unbound marks can run a
  command from `[marks]`.
- **Layouts** are binary split trees of containers (`single`, `stack`, `queue`, `grid`, `full`,
  `empty`) stored in registers. They tile a sheet on demand; new views float until restacked.
  `layout-exchange-view-*` and `layout-cycle-view-*` work on the layout order.
- **Modes**: group-assign, mark-assign, mark-select, mark-switch-select, sheet-assign, pan,
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
| Alt+E | `menu-files`: a file manager like ranger |

Up/Down, Tab or Ctrl+N/P select, Return or a click accepts, and Escape, Ctrl+G or a click elsewhere
closes it. Fonts and colors are under `[bar]` in the config.

The file manager shows the parent directory, the current one and a preview of the selected entry
(a directory's contents, a text file's first lines, or an image's size), and reopens where it was
closed. Its keys follow ranger's:

| Keys | Does |
| --- | --- |
| j/k, Up/Down | select the next/previous entry |
| J/K, Page Down/Up, Ctrl+D/U | move a page or half a page |
| h, Left | go to the parent directory |
| l, Right, Return | enter the directory, or open the file with `opener` (`xdg-open` by default) |
| gg/G | go to the first/last entry |
| gh or ~, g/ | go home, or to the root |
| zh, Backspace | show or hide dotfiles |
| / | filter the directory (Return keeps the filter, Escape clears it) |
| S | open the `terminal` action in the directory |
| q, Escape | close |

## Differences from hikari

- Instead of indicator bars on each view, one bar shows the focused view and what modes have typed.
- A single output on a single GPU, with no multi-monitor support. No layer-shell or screen locking.
- X11 apps run through [xwayland-satellite](https://github.com/Supreeeme/xwayland-satellite) when
  it is installed, started on the first X11 connection as niri does, rather than built-in XWayland.
  Their app id is their WM_CLASS, e.g. `XTerm`.
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
move/resize, snapping, input grabs, reload, quit, the bar and menu, wallpapers and the wallpaper portal.

They need sway and grim, so plain `cargo test` skips them. Run them with:

```sh
nix develop -c cargo test --test e2e -- --ignored   # uses your GPU, ~15s
nix flake check                                     # in the Nix sandbox with software rendering,
                                                    # plus a VM that logs in through GDM (needs KVM)
```

When a test fails it names the pixel that was wrong and keeps a directory with the screenshot
(`failure.ppm`) and the sway and Rose logs.

{
  description = "Rose: a basic Wayland compositor built on Smithay";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # Loaded with dlopen at runtime by winit and the EGL renderer.
      runtimeLibs =
        pkgs: with pkgs; [
          libGL
          libxkbcommon
          wayland
          libx11
          libxcursor
          libxi
          libxcb
          libgbm
          libinput
          libdrm
          seatd
          systemd
        ];
    in
    {
      nixosModules.default = import ./nix/nixos-module.nix self;
      homeManagerModules.default = import ./nix/home-manager-module.nix self;

      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "rose";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            pkg-config
            makeWrapper
          ];
          buildInputs = runtimeLibs pkgs;

          # Make the runtime libraries findable and provide the default terminal.
          postInstall = ''
            install -Dm644 resources/rose.desktop -t $out/share/wayland-sessions
            substituteInPlace $out/share/wayland-sessions/rose.desktop --replace-fail @rose@ $out/bin/rose
            install -Dm644 resources/rose.portal -t $out/share/xdg-desktop-portal/portals
            install -Dm644 resources/rose-portals.conf -t $out/share/xdg-desktop-portal
            wrapProgram $out/bin/rose \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)} \
              --suffix PATH : ${
                pkgs.lib.makeBinPath [
                  pkgs.foot
                  pkgs.xwayland-satellite
                  pkgs.dbus
                ]
              }
          '';

          # For services.displayManager.sessionPackages.
          passthru.providedSessions = [ "rose" ];

          meta.mainProgram = "rose";
        };
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = pkgs.lib.getExe self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        };
      });

      # The end-to-end tests, run with software rendering so they work without a GPU.
      checks = forAllSystems (pkgs: {
        session = import ./nix/session-test.nix { inherit pkgs self; };

        e2e = self.packages.${pkgs.stdenv.hostPlatform.system}.default.overrideAttrs (old: {
          pname = "rose-e2e";
          nativeCheckInputs = (old.nativeCheckInputs or [ ]) ++ [
            # Unwrapped, since the wrapper needs a D-Bus session.
            pkgs.sway-unwrapped
            pkgs.grim
            pkgs.dbus
            # For the X11 test.
            pkgs.xwayland-satellite
            pkgs.xterm
          ];
          cargoTestFlags = [
            "--test"
            "e2e"
          ];
          checkFlags = [ "--ignored" ];
          preCheck = ''
            export LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)}
            export __EGL_VENDOR_LIBRARY_DIRS=${pkgs.mesa}/share/glvnd/egl_vendor.d
            export LIBGL_ALWAYS_SOFTWARE=1 WLR_RENDERER=pixman
            export FONTCONFIG_FILE=${pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; }}
          '';
        });
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.default ];
          packages = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer
            foot
            # For the end-to-end tests.
            sway
            grim
            dbus
            xwayland-satellite
            xterm
          ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
        };
      });
    };
}

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
        ];
    in
    {
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
            wrapProgram $out/bin/rose \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)} \
              --suffix PATH : ${pkgs.lib.makeBinPath [ pkgs.foot ]}
          '';

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
        e2e = self.packages.${pkgs.stdenv.hostPlatform.system}.default.overrideAttrs (old: {
          pname = "rose-e2e";
          nativeCheckInputs = (old.nativeCheckInputs or [ ]) ++ [
            # Unwrapped, since the wrapper needs a D-Bus session.
            pkgs.sway-unwrapped
            pkgs.grim
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
          ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
        };
      });
    };
}

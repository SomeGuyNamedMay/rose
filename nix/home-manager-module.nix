self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.rose;
  toml = pkgs.formats.toml { };
  defaults = fromTOML (builtins.readFile ../config.toml);
in
{
  options.programs.rose = {
    enable = lib.mkEnableOption "Rose, a Wayland compositor";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "rose.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "The Rose package to use.";
    };

    settings = lib.mkOption {
      type = toml.type;
      default = { };
      example = lib.literalExpression ''
        {
          ui.gap = 8;
          bar.font-size = 16;
          bindings.keyboard."AS+Return" = "action-terminal";
        }
      '';
      description = ''
        Configuration written to {file}`$XDG_CONFIG_HOME/rose/config.toml`. See the
        {file}`config.toml` in Rose's repository for the available options. If this is empty and
        {option}`programs.rose.mergeDefaults` is left on, no file is written and Rose uses its
        built-in configuration.
      '';
    };

    mergeDefaults = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Whether to merge {option}`programs.rose.settings` over Rose's built-in configuration.
        Rose doesn't do this itself: a config file replaces the built-in configuration, including
        all of its key bindings. Turn this off to write exactly what is in
        {option}`programs.rose.settings`.
      '';
    };

    session.enable = lib.mkEnableOption ''
      the `rose-session` systemd target, which binds `graphical-session.target` while Rose is the
      login session. Listing Rose in a display manager takes the NixOS module
    '' // {
      default = true;
    };

    portal.enable = lib.mkEnableOption ''
      Rose's backend for the wallpaper portal. On NixOS, prefer the option of the NixOS module
    '';
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      { home.packages = [ cfg.package ]; }

      (lib.mkIf (cfg.settings != { } || !cfg.mergeDefaults) {
        xdg.configFile."rose/config.toml".source = toml.generate "rose-config.toml" (
          if cfg.mergeDefaults then lib.recursiveUpdate defaults cfg.settings else cfg.settings
        );
      })

      (lib.mkIf cfg.session.enable {
        systemd.user.targets.rose-session.Unit = {
          Description = "Rose session";
          BindsTo = [ "graphical-session.target" ];
          Wants = [ "graphical-session-pre.target" ];
          After = [ "graphical-session-pre.target" ];
        };
      })

      (lib.mkIf cfg.portal.enable {
        xdg.portal = {
          enable = true;
          # The gtk backend provides the Access dialog that the wallpaper portal requires.
          extraPortals = [
            cfg.package
            pkgs.xdg-desktop-portal-gtk
          ];
          configPackages = [ cfg.package ];
        };
      })
    ]
  );
}

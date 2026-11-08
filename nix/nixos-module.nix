self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.rose;
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

    session.enable = lib.mkEnableOption "Rose as a session in display managers" // {
      default = true;
    };

    portal.enable = lib.mkEnableOption "Rose's backend for the wallpaper portal" // {
      default = true;
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      { environment.systemPackages = [ cfg.package ]; }

      (lib.mkIf cfg.session.enable {
        services.displayManager.sessionPackages = [ cfg.package ];
        services.graphical-desktop.enable = true;
        security.polkit.enable = true;
        programs.dconf.enable = lib.mkDefault true;

        # Started by Rose when it runs on the hardware, for services of the graphical session.
        systemd.user.targets.rose-session = {
          description = "Rose session";
          bindsTo = [ "graphical-session.target" ];
          wants = [ "graphical-session-pre.target" ];
          after = [ "graphical-session-pre.target" ];
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

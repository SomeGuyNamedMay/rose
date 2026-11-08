# Logs in to Rose through GDM in a VM, then uses it through the emulated keyboard and screen.
{ pkgs, self }:
pkgs.testers.runNixOSTest {
  name = "rose-session";

  nodes.machine = {
    imports = [ self.nixosModules.default ];

    programs.rose.enable = true;

    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
    };
    services.displayManager = {
      gdm.enable = true;
      autoLogin = {
        enable = true;
        user = "alice";
      };
      defaultSession = "rose";
    };

    virtualisation = {
      memorySize = 4096;
      cores = 2;
      qemu.options = [
        "-vga none"
        "-device virtio-gpu-pci"
      ];
    };
  };

  testScript = ''
    def windows():
        return int(machine.succeed("pgrep -u alice -c foot || true").strip())

    machine.wait_for_unit("multi-user.target")
    machine.wait_until_succeeds("pgrep -u alice -x .rose-wrapped", timeout=120)
    machine.wait_until_succeeds("test -S /run/user/1000/wayland-1", timeout=60)

    with subtest("the session starts its systemd target and first window"):
        machine.wait_until_succeeds(
            "systemctl --user -M alice@ is-active rose-session.target", timeout=60
        )
        machine.wait_until_succeeds("pgrep -u alice foot", timeout=60)
        machine.sleep(3)
        machine.screenshot("session")

    with subtest("the keyboard reaches Rose"):
        machine.send_key("alt-ret")
        machine.wait_until_succeeds("test $(pgrep -u alice -c foot) -ge 2", timeout=30)
        machine.sleep(2)
        machine.screenshot("two-windows")

    with subtest("the session survives switching VTs"):
        vt = machine.succeed(
            "loginctl show-session $(loginctl list-sessions --no-legend | "
            "awk '$3 == \"alice\" {print $1; exit}') -p VTNr --value"
        ).strip()
        other = "8" if vt != "8" else "9"
        machine.send_key(f"ctrl-alt-f{other}")
        machine.wait_until_succeeds(f"test $(cat /sys/class/tty/tty0/active) = tty{other}", timeout=30)
        machine.sleep(2)
        machine.screenshot("other-vt")
        machine.succeed(f"chvt {vt}")
        machine.sleep(3)
        machine.screenshot("back")
        machine.succeed("pgrep -u alice -x .rose-wrapped")
        before = windows()
        machine.send_key("alt-ret")
        machine.wait_until_succeeds(f"test $(pgrep -u alice -c foot) -gt {before}", timeout=30)

    with subtest("quitting ends the session"):
        machine.send_key("ctrl-alt-q")
        machine.sleep(1)
        machine.send_key("ctrl-alt-q")
        machine.wait_until_fails("pgrep -u alice -x .rose-wrapped", timeout=60)
  '';
}

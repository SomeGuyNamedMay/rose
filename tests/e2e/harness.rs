//! Runs Rose nested inside a headless sway, drives it with virtual input and checks the result
//! in screenshots.

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use crate::{client::Client, input::VirtualInput};

pub const OUTPUT_WIDTH: u32 = 1200;
pub const OUTPUT_HEIGHT: u32 = 800;

/// How long to wait for the screen to show an expected result.
const TIMEOUT: Duration = Duration::from_secs(5);

/// The config every test starts from; tests can append to it.
const BASE_CONFIG: &str = include_str!("config.toml");

pub type Rgb = [u8; 3];
pub const BACKGROUND: Rgb = [0x28, 0x2C, 0x34];
pub const ACTIVE: Rgb = [0xFF, 0xFF, 0xFF];
pub const INACTIVE: Rgb = [0x46, 0x54, 0x57];
pub const GROUPED: Rgb = [0xFD, 0xAF, 0x53];
pub const INSERT: Rgb = [0xE3, 0xC3, 0xFA];
pub const CONFLICT: Rgb = [0xED, 0x6B, 0x32];

/// Sessions run one at a time so they don't compete for the GPU and slow each other down.
static SERIAL: Mutex<()> = Mutex::new(());

pub struct Session {
    dir: PathBuf,
    sway: Child,
    bus: Child,
    rose: Child,
    rose_socket: PathBuf,
    pub config_path: PathBuf,
    pub input: VirtualInput,
    _serial: MutexGuard<'static, ()>,
}

impl Session {
    /// Starts sway and Rose with the base config plus `extra_config`.
    pub fn start(extra_config: &str) -> Self {
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

        // Socket paths must be short, so use the system temp dir rather than the target dir.
        let dir =
            std::env::temp_dir().join(format!("rose-e2e-{}-{}", std::process::id(), unique()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sway.conf"),
            format!("output HEADLESS-1 resolution {OUTPUT_WIDTH}x{OUTPUT_HEIGHT}\ndefault_border none\n"),
        )
        .unwrap();

        let sway = Command::new("sway")
            .arg("--config")
            .arg(dir.join("sway.conf"))
            .env("XDG_RUNTIME_DIR", &dir)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(log_file(&dir, "sway.log"))
            .stderr(log_file(&dir, "sway.log"))
            .spawn()
            .expect("failed to start sway; the e2e tests need sway and grim on PATH (use `nix develop`)");
        let sway_socket = dir.join("wayland-1");
        wait_for(
            || sway_socket.exists(),
            "sway to start",
            &dir.join("sway.log"),
        );

        // The bar would change the geometry most tests expect, so it's off unless a test
        // configures it.
        let bar = if extra_config.contains("[bar]") {
            ""
        } else {
            "[bar]\nenabled = false\n"
        };
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, format!("{BASE_CONFIG}\n{bar}{extra_config}")).unwrap();

        // Rose offers a portal on the session bus, so give it a bus of its own.
        let bus_socket = dir.join("bus");
        let bus_config = dir.join("bus.conf");
        std::fs::write(
            &bus_config,
            format!(
                "<busconfig><type>session</type>\
                 <listen>unix:path={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\"/><allow eavesdrop=\"true\"/>\
                 <allow own=\"*\"/></policy></busconfig>",
                bus_socket.display()
            ),
        )
        .unwrap();
        let bus = Command::new("dbus-daemon")
            .arg("--nofork")
            .arg(format!("--config-file={}", bus_config.display()))
            .stdout(log_file(&dir, "dbus.log"))
            .stderr(log_file(&dir, "dbus.log"))
            .spawn()
            .expect(
                "failed to start dbus-daemon; the e2e tests need it on PATH (use `nix develop`)",
            );
        wait_for(
            || bus_socket.exists(),
            "the session bus to start",
            &dir.join("dbus.log"),
        );

        let rose = Command::new(env!("CARGO_BIN_EXE_rose"))
            .arg("--config")
            .arg(&config_path)
            .env("XDG_RUNTIME_DIR", &dir)
            .env("WAYLAND_DISPLAY", "wayland-1")
            .env("NO_COLOR", "1")
            .env("XDG_STATE_HOME", &dir)
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", bus_socket.display()),
            )
            .env_remove("DISPLAY")
            .stdout(log_file(&dir, "rose.log"))
            .stderr(log_file(&dir, "rose.log"))
            .spawn()
            .unwrap();
        let rose_socket = dir.join("wayland-2");
        wait_for(
            || rose_socket.exists(),
            "rose to start",
            &dir.join("rose.log"),
        );

        let input = VirtualInput::connect(&sway_socket, &keymap());
        let session = Self {
            dir,
            sway,
            bus,
            rose,
            rose_socket,
            config_path,
            input,
            _serial: serial,
        };
        // Rose's window fills the output once sway has tiled it.
        session.wait_pixel(OUTPUT_WIDTH - 2, OUTPUT_HEIGHT / 2, BACKGROUND);
        session
    }

    /// Opens a test window. Waits until the window shows at `visible_at`.
    pub fn client(&self, app_id: &str, color: Rgb, visible_at: (u32, u32)) -> Client {
        let client = Client::spawn(&self.rose_socket, app_id, color);
        self.wait_pixel(visible_at.0, visible_at.1, color);
        client
    }

    /// The address of the session bus Rose is on.
    pub fn bus_address(&self) -> String {
        format!("unix:path={}", self.dir.join("bus").display())
    }

    pub fn screenshot(&self) -> Screenshot {
        let output = Command::new("grim")
            .args(["-t", "ppm", "-"])
            .env("XDG_RUNTIME_DIR", &self.dir)
            .env("WAYLAND_DISPLAY", "wayland-1")
            .stderr(Stdio::inherit())
            .output()
            .expect("failed to run grim");
        assert!(output.status.success(), "grim failed");
        Screenshot::parse(output.stdout)
    }

    /// Waits until the pixel at (`x`, `y`) has `color`, failing the test after a timeout.
    #[track_caller]
    pub fn wait_pixel(&self, x: u32, y: u32, color: Rgb) {
        let start = Instant::now();
        loop {
            let shot = self.screenshot();
            let actual = shot.pixel(x, y);
            if close(actual, color) {
                return;
            }
            if start.elapsed() > TIMEOUT {
                let path = self.dir.join("failure.ppm");
                std::fs::write(&path, shot.ppm()).unwrap();
                panic!(
                    "pixel ({x}, {y}) is {} instead of {}; screenshot and logs are in {}",
                    hex(actual),
                    hex(color),
                    self.dir.display()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Waits for Rose to exit, returning whether it did before the timeout.
    pub fn wait_for_exit(&mut self) -> bool {
        let start = Instant::now();
        while start.elapsed() < TIMEOUT {
            if self.rose.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// Writes a new config file, for testing `reload`.
    pub fn write_config(&self, contents: &str) {
        std::fs::write(&self.config_path, contents).unwrap();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.rose.kill();
        let _ = self.rose.wait();
        let _ = self.sway.kill();
        let _ = self.sway.wait();
        let _ = self.bus.kill();
        let _ = self.bus.wait();
        if std::thread::panicking() {
            eprintln!("keeping {} for debugging", self.dir.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

pub struct Screenshot {
    width: u32,
    height: u32,
    /// RGB rows, top to bottom.
    data: Vec<u8>,
}

impl Screenshot {
    /// Parses grim's binary PPM (P6) output.
    fn parse(bytes: Vec<u8>) -> Self {
        let mut fields = Vec::new();
        let mut pos = 0;
        while fields.len() < 4 {
            while bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            let start = pos;
            while !bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            fields.push(String::from_utf8_lossy(&bytes[start..pos]).into_owned());
        }
        assert_eq!(fields[0], "P6", "grim did not produce a PPM");
        assert_eq!(fields[3], "255");
        Self {
            width: fields[1].parse().unwrap(),
            height: fields[2].parse().unwrap(),
            data: bytes[pos + 1..].to_vec(),
        }
    }

    pub fn pixel(&self, x: u32, y: u32) -> Rgb {
        assert!(
            x < self.width && y < self.height,
            "({x}, {y}) is outside the screenshot"
        );
        let i = ((y * self.width + x) * 3) as usize;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    fn ppm(&self) -> Vec<u8> {
        let mut out = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        out.extend(&self.data);
        out
    }
}

fn close(a: Rgb, b: Rgb) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 2)
}

fn hex(c: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

fn log_file(dir: &Path, name: &str) -> std::fs::File {
    std::fs::File::options()
        .create(true)
        .append(true)
        .open(dir.join(name))
        .unwrap()
}

/// Waits for a process to start, showing the end of its log if it doesn't.
fn wait_for(mut condition: impl FnMut() -> bool, what: &str, log: &Path) {
    let start = Instant::now();
    while !condition() {
        if start.elapsed() > TIMEOUT {
            let log = std::fs::read_to_string(log).unwrap_or_default();
            let tail: Vec<&str> = log.lines().rev().take(20).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            panic!(
                "timed out waiting for {what}; its log ends with:\n{}",
                tail.join("\n")
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A US keymap whose keycodes are evdev codes plus 8.
fn keymap() -> String {
    use xkbcommon::xkb;
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_names(
        &context,
        "",
        "",
        "us",
        "",
        None,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .expect("failed to compile a US keymap");
    keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)
}

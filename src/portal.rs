//! The backend of the wallpaper portal, so that applications can ask to change Rose's wallpaper.
//! The image is copied into Rose's state directory, and is used until the wallpaper in the config
//! changes.

use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use futures_channel::oneshot;
use image::ImageReader;
use percent_encoding::percent_decode_str;
use smithay::reexports::calloop::channel::Sender;
use zbus::{connection::Builder, ObjectPath, OwnedVariant};

use crate::{
    action::MenuSource,
    menu::{Item, Menu, Target},
    wm::Mode,
    Rose,
};

const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.rose";
const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

const IMAGE: &str = "wallpaper";
/// Holds the config's wallpaper path from when the image was set.
const CONFIG_RECORD: &str = "wallpaper.config";

const SUCCESS: u32 = 0;
const CANCELLED: u32 = 1;
const FAILED: u32 = 2;

/// An application's request. Dropping it without accepting it declines it.
pub struct Request {
    app_id: String,
    name: String,
    /// The portal only asks the user itself when no preview was requested.
    confirm: bool,
    /// Our copy of the image, until it is accepted.
    staged: PathBuf,
    reply: Mutex<Option<oneshot::Sender<u32>>>,
}

impl Request {
    fn respond(&self, response: u32) {
        if let Some(reply) = self.reply.lock().unwrap().take() {
            let _ = reply.send(response);
        }
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.staged);
    }
}

struct Portal {
    sender: Sender<Arc<Request>>,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Wallpaper")]
impl Portal {
    #[zbus(name = "SetWallpaperURI")]
    async fn set_wallpaper_uri(
        &self,
        _handle: ObjectPath<'_>,
        app_id: String,
        _parent_window: String,
        uri: String,
        options: HashMap<String, OwnedVariant>,
    ) -> u32 {
        // Rose has no lock screen to set it on.
        let target = options
            .get("set-on")
            .and_then(|v| v.downcast_ref::<&str>().ok());
        if target == Some("lockscreen") {
            return FAILED;
        }
        let confirm = options
            .get("show-preview")
            .and_then(|v| v.downcast_ref::<bool>().ok())
            .unwrap_or(false);

        let staged = file_path(&uri)
            .ok_or_else(|| "only file URIs are supported".to_string())
            .and_then(|path| Ok((stage(&path)?, path)));
        let (staged, path) = match staged {
            Ok(staged) => staged,
            Err(err) => {
                tracing::warn!("can't set the wallpaper to {uri}: {err}");
                return FAILED;
            }
        };

        let (reply, response) = oneshot::channel();
        let request = Request {
            app_id,
            name: path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            confirm,
            staged,
            reply: Mutex::new(Some(reply)),
        };
        if self.sender.send(Arc::new(request)).is_err() {
            return FAILED;
        }
        response.await.unwrap_or(CANCELLED)
    }
}

/// Offers the portal on the session bus from another thread. Requests arrive through `sender`.
pub fn spawn(sender: Sender<Arc<Request>>) {
    std::thread::spawn(move || {
        zbus::block_on(async move {
            let connection = Builder::session()
                .name(BUS_NAME)
                .serve_at(OBJECT_PATH, Portal { sender })
                .build()
                .await;
            match connection {
                // Kept for as long as Rose runs.
                Ok(_connection) => std::future::pending::<()>().await,
                Err(err) => tracing::warn!("the wallpaper portal is unavailable: {err}"),
            }
        });
    });
}

fn file_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    rest.starts_with('/')
        .then(|| PathBuf::from(OsString::from_vec(percent_decode_str(rest).collect())))
}

fn state_dir() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/state")))?;
    Some(state.join("rose"))
}

/// Copies an image into the state directory, as applications' files may not stay around.
fn stage(path: &Path) -> Result<PathBuf, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    // Reading anything else, like a named pipe, might never finish.
    if !fs::metadata(path).map_err(|e| e.to_string())?.is_file() {
        return Err("not a regular file".into());
    }
    let reader = ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?;
    if reader.format().is_none() {
        return Err("not an image".into());
    }
    let dir = state_dir().ok_or("no state directory")?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let staged = dir.join(format!(
        "wallpaper.{}.new",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::copy(path, &staged).map_err(|e| e.to_string())?;
    Ok(staged)
}

fn config_record(configured: Option<&Path>) -> &[u8] {
    configured.map_or(&[], |p| p.as_os_str().as_bytes())
}

/// The image set through the portal, unless the config's wallpaper has changed since.
pub fn current(configured: Option<&Path>) -> Option<PathBuf> {
    let dir = state_dir()?;
    let recorded = fs::read(dir.join(CONFIG_RECORD)).ok()?;
    let image = dir.join(IMAGE);
    (recorded == config_record(configured) && image.is_file()).then_some(image)
}

fn install(staged: &Path, configured: Option<&Path>) -> std::io::Result<()> {
    let dir = state_dir().ok_or(std::io::ErrorKind::NotFound)?;
    fs::write(dir.join(CONFIG_RECORD), config_record(configured))?;
    fs::rename(staged, dir.join(IMAGE))
}

impl Rose {
    pub fn handle_portal_request(&mut self, request: Arc<Request>) {
        if !request.confirm {
            self.accept_wallpaper(&request);
            return;
        }
        // Dropping the request declines it.
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        let app = if request.app_id.is_empty() {
            "an application"
        } else {
            &request.app_id
        };
        let details = format!("{} from {app}", request.name);
        // Keeping the wallpaper comes first, so a stray Return doesn't accept.
        let items = vec![
            Item {
                columns: vec!["Keep the current wallpaper".into(), String::new()],
                target: Target::Wallpaper(None),
            },
            Item {
                columns: vec!["Set as wallpaper".into(), details],
                target: Target::Wallpaper(Some(request)),
            },
        ];
        self.menu = Some(Menu::new(
            MenuSource::Wallpaper,
            items,
            self.config.menu.lines,
        ));
        self.mode = Mode::Menu;
    }

    pub fn accept_wallpaper(&mut self, request: &Request) {
        let configured = self.config.ui.wallpaper.path.as_deref();
        match install(&request.staged, configured) {
            Ok(()) => {
                self.wallpaper.configure(self.wallpaper_path());
                request.respond(SUCCESS);
            }
            Err(err) => {
                tracing::warn!("failed to save the wallpaper: {err}");
                request.respond(FAILED);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_become_paths() {
        let path = |uri| file_path(uri).map(|p| p.display().to_string());
        assert_eq!(path("file:///a/b%20c.png").as_deref(), Some("/a/b c.png"));
        assert_eq!(path("file://localhost/a.png").as_deref(), Some("/a.png"));
        assert_eq!(path("https://example.com/a.png"), None);
        assert_eq!(path("file://host/a.png"), None);
    }
}

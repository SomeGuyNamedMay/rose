//! The wallpaper: a still image or an animated GIF, PNG or WebP. Images are decoded off the main
//! thread, and scaled to the output by the renderer.

use std::{
    error::Error,
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use image::{
    codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder},
    AnimationDecoder, ImageFormat, ImageReader,
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind,
            },
            gles::GlesRenderer,
        },
    },
    reexports::calloop::channel::Sender,
    utils::{Logical, Point, Rectangle, Size, Transform},
};

use crate::{config::WallpaperMode, render::RoseRenderElement, Rose};

/// Frames shorter than this are shown for `DEFAULT_DELAY` instead, like browsers do.
const MIN_DELAY: Duration = Duration::from_millis(20);
const DEFAULT_DELAY: Duration = Duration::from_millis(100);

/// A decoded frame in RGBA order, sent from the decoding thread.
pub struct RawFrame {
    data: Vec<u8>,
    size: (u32, u32),
    delay: Duration,
}

pub type Decoded = (u64, Result<Vec<RawFrame>, String>);

struct Frame {
    buffer: MemoryRenderBuffer,
    size: Size<i32, Logical>,
    /// When this frame stops being shown, from the start of the animation.
    end: Duration,
}

pub struct Wallpaper {
    /// The file shown or being loaded, and its modification time, so a reload only re-reads it
    /// when it changed.
    source: Option<(PathBuf, Option<SystemTime>)>,
    /// Identifies the latest load, so results of an older one are ignored.
    generation: u64,
    sender: Sender<Decoded>,
    frames: Vec<Frame>,
    started: Instant,
}

impl Wallpaper {
    pub fn new(sender: Sender<Decoded>) -> Self {
        Self {
            source: None,
            generation: 0,
            sender,
            frames: Vec::new(),
            started: Instant::now(),
        }
    }

    /// Starts loading `path`, keeping the current wallpaper until it's ready.
    pub fn configure(&mut self, path: Option<PathBuf>) {
        let source = path.map(|path| {
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            (path, modified)
        });
        if source == self.source {
            return;
        }
        self.source = source;
        self.generation += 1;
        let Some((path, _)) = &self.source else {
            self.frames.clear();
            return;
        };

        let (generation, path, sender) = (self.generation, path.clone(), self.sender.clone());
        std::thread::spawn(move || {
            let result = decode(&path).map_err(|err| format!("{}: {err}", path.display()));
            let _ = sender.send((generation, result));
        });
    }

    pub fn set_frames(&mut self, generation: u64, result: Result<Vec<RawFrame>, String>) {
        if generation != self.generation {
            return;
        }
        let raw = match result {
            Ok(raw) => raw,
            Err(err) => {
                tracing::error!("failed to load the wallpaper: {err}");
                self.frames.clear();
                return;
            }
        };
        let mut end = Duration::ZERO;
        self.frames = raw
            .into_iter()
            .map(|frame| {
                let (w, h) = (frame.size.0 as i32, frame.size.1 as i32);
                end += frame.delay;
                Frame {
                    buffer: MemoryRenderBuffer::from_slice(
                        &frame.data,
                        Fourcc::Abgr8888,
                        (w, h),
                        1,
                        Transform::Normal,
                        None,
                    ),
                    size: (w, h).into(),
                    end,
                }
            })
            .collect();
        self.started = Instant::now();
    }

    /// The frame to show now.
    fn current(&self) -> Option<&Frame> {
        let period = self.frames.last()?.end;
        if self.frames.len() == 1 || period.is_zero() {
            return self.frames.first();
        }
        let t =
            Duration::from_nanos((self.started.elapsed().as_nanos() % period.as_nanos()) as u64);
        self.frames
            .iter()
            .find(|f| f.end > t)
            .or(self.frames.last())
    }

    fn element(
        &mut self,
        renderer: &mut GlesRenderer,
        output: Rectangle<i32, Logical>,
        mode: WallpaperMode,
    ) -> Option<RoseRenderElement> {
        let frame = self.current()?;
        let placement = placement(output, frame.size, mode);
        let element = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            placement.dest.loc.to_f64().to_physical(1.0),
            &frame.buffer,
            None,
            Some(placement.src),
            Some(placement.dest.size),
            Kind::Unspecified,
        );
        match element {
            Ok(element) => Some(element.into()),
            Err(err) => {
                // Usually the image is larger than the GPU's maximum texture size.
                tracing::error!("failed to upload the wallpaper: {err:?}");
                self.frames.clear();
                None
            }
        }
    }
}

#[derive(Debug, PartialEq)]
struct Placement {
    /// The part of the image that's shown.
    src: Rectangle<f64, Logical>,
    /// Where it's drawn.
    dest: Rectangle<i32, Logical>,
}

/// How an image of `image` size is drawn on `output`.
fn placement(
    output: Rectangle<i32, Logical>,
    image: Size<i32, Logical>,
    mode: WallpaperMode,
) -> Placement {
    let (ow, oh) = (output.size.w as f64, output.size.h as f64);
    let (iw, ih) = (image.w.max(1) as f64, image.h.max(1) as f64);
    let whole = Rectangle::from_size(image.to_f64());
    let centered = |size: Size<i32, Logical>| {
        let loc =
            output.loc + Point::from(((output.size.w - size.w) / 2, (output.size.h - size.h) / 2));
        Rectangle::new(loc, size)
    };
    match mode {
        WallpaperMode::Stretch => Placement {
            src: whole,
            dest: output,
        },
        WallpaperMode::Fill => {
            let scale = (ow / iw).max(oh / ih);
            let (w, h) = (ow / scale, oh / scale);
            Placement {
                src: Rectangle::new(((iw - w) / 2.0, (ih - h) / 2.0).into(), (w, h).into()),
                dest: output,
            }
        }
        WallpaperMode::Fit => {
            let scale = (ow / iw).min(oh / ih);
            let size = Size::from(((iw * scale).round() as i32, (ih * scale).round() as i32));
            Placement {
                src: whole,
                dest: centered(size),
            }
        }
        WallpaperMode::Center => Placement {
            src: whole,
            dest: centered(image),
        },
    }
}

fn decode(path: &Path) -> Result<Vec<RawFrame>, Box<dyn Error>> {
    let open = || File::open(path).map(BufReader::new);
    let reader = ImageReader::new(open()?).with_guessed_format()?;
    let frames = match reader.format() {
        Some(ImageFormat::Gif) => Some(animation(GifDecoder::new(open()?)?)?),
        Some(ImageFormat::Png) => {
            let decoder = PngDecoder::new(open()?)?;
            if decoder.is_apng()? {
                Some(animation(decoder.apng()?)?)
            } else {
                None
            }
        }
        Some(ImageFormat::WebP) => {
            let decoder = WebPDecoder::new(open()?)?;
            if decoder.has_animation() {
                Some(animation(decoder)?)
            } else {
                None
            }
        }
        _ => None,
    };
    let frames = match frames {
        Some(frames) => frames,
        None => {
            let image = reader.decode()?.into_rgba8();
            vec![RawFrame {
                size: image.dimensions(),
                data: image.into_raw(),
                delay: Duration::ZERO,
            }]
        }
    };
    if frames.is_empty() {
        return Err("the animation has no frames".into());
    }
    Ok(frames)
}

fn animation<'a>(decoder: impl AnimationDecoder<'a>) -> Result<Vec<RawFrame>, Box<dyn Error>> {
    decoder
        .into_frames()
        .map(|frame| {
            let frame = frame?;
            let (numer, denom) = frame.delay().numer_denom_ms();
            let delay = Duration::from_secs_f64(numer as f64 / denom.max(1) as f64 / 1000.0);
            let image = frame.into_buffer();
            Ok(RawFrame {
                size: image.dimensions(),
                data: image.into_raw(),
                delay: if delay < MIN_DELAY {
                    DEFAULT_DELAY
                } else {
                    delay
                },
            })
        })
        .collect()
}

impl Rose {
    /// The configured wallpaper's path, with `~/` expanded and relative to the config file.
    pub fn wallpaper_path(&self) -> Option<PathBuf> {
        let path = self.config.ui.wallpaper.path.as_ref()?;
        let path = match (path.strip_prefix("~"), std::env::var_os("HOME")) {
            (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
            _ => path.clone(),
        };
        Some(match self.config_path.parent() {
            Some(dir) if path.is_relative() => dir.join(path),
            _ => path,
        })
    }

    pub fn wallpaper_element(&mut self, renderer: &mut GlesRenderer) -> Option<RoseRenderElement> {
        let output = self.output_rect();
        let mode = self.config.ui.wallpaper.mode;
        self.wallpaper.element(renderer, output, mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placements() {
        let output = Rectangle::from_size((200, 100).into());
        let image = Size::from((100, 100));

        let whole = Rectangle::from_size((100.0, 100.0).into());

        assert_eq!(
            placement(output, image, WallpaperMode::Fill),
            Placement {
                src: Rectangle::new((0.0, 25.0).into(), (100.0, 50.0).into()),
                dest: output,
            }
        );
        assert_eq!(
            placement(output, image, WallpaperMode::Fit),
            Placement {
                src: whole,
                dest: Rectangle::new((50, 0).into(), (100, 100).into()),
            }
        );
        assert_eq!(
            placement(output, image, WallpaperMode::Stretch),
            Placement {
                src: whole,
                dest: output
            }
        );
        assert_eq!(
            placement(output, (300, 50).into(), WallpaperMode::Center).dest,
            Rectangle::new((-50, 25).into(), (300, 50).into())
        );
    }
}

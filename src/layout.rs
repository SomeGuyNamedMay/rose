//! Tiling layouts, following hikari's layout model.
//!
//! A layout is a tree of binary splits whose leaves are containers. Views are handed out in
//! layout order: the dominant side of a split (the side named first in the config) fills up
//! before the other side gets any views.

use smithay::utils::{Logical, Point, Rectangle, Size};

/// How many views a container takes when the config doesn't say.
const DEFAULT_VIEWS: usize = 256;

#[derive(Clone, Debug)]
pub enum Layout {
    Split(Box<Split>),
    Container { views: usize, kind: Container },
}

#[derive(Clone, Debug)]
pub struct Split {
    /// `true` for `left`/`right` splits, `false` for `top`/`bottom`.
    pub vertical: bool,
    /// Whether the left/top side is dominant.
    pub first_dominant: bool,
    /// Fraction of the space given to the dominant side.
    pub scale: Scale,
    /// The left or top side.
    pub first: Layout,
    /// The right or bottom side.
    pub second: Layout,
}

#[derive(Clone, Copy, Debug)]
pub enum Scale {
    Fixed(f64),
    /// Follows the size of the first view on the dominant side, within bounds.
    Dynamic {
        min: f64,
        max: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    /// Takes no views and leaves its space empty.
    Empty,
    /// One view fills the container.
    Single,
    /// Every view fills the container, stacked on top of each other.
    Full,
    /// Views share the height equally, top to bottom.
    Stack,
    /// Views share the width equally, left to right.
    Queue,
    /// Views are arranged in equal rows and columns.
    Grid,
}

impl Layout {
    pub fn parse(value: &toml::Value) -> Result<Self, String> {
        match value {
            toml::Value::String(name) => Ok(Self::Container {
                views: DEFAULT_VIEWS,
                kind: Container::parse(name)?,
            }),
            toml::Value::Table(table) if table.contains_key("layout") => {
                let mut views = DEFAULT_VIEWS;
                let mut kind = None;
                for (key, value) in table {
                    match (key.as_str(), value) {
                        ("views", toml::Value::Integer(n)) if *n >= 0 => views = *n as usize,
                        ("layout", toml::Value::String(name)) => {
                            kind = Some(Container::parse(name)?)
                        }
                        _ => return Err(format!("invalid container key `{key}`")),
                    }
                }
                Ok(Self::Container {
                    views,
                    kind: kind.unwrap(),
                })
            }
            toml::Value::Table(table) => {
                let mut scale = Scale::Fixed(0.5);
                // (side name, layout) in the order they appear, so the first one is dominant.
                let mut sides = Vec::new();
                for (key, value) in table {
                    match key.as_str() {
                        "scale" => scale = Scale::parse(value)?,
                        "left" | "right" | "top" | "bottom" => {
                            sides.push((key.as_str(), Layout::parse(value)?))
                        }
                        _ => return Err(format!("invalid split key `{key}`")),
                    }
                }

                let [(dominant, a), (_, b)]: [(&str, Layout); 2] = sides
                    .try_into()
                    .map_err(|_| "a split needs exactly two sides".to_string())?;
                let names: Vec<&str> = table
                    .keys()
                    .map(String::as_str)
                    .filter(|k| *k != "scale")
                    .collect();
                let vertical = match (names[0], names[1]) {
                    ("left", "right") | ("right", "left") => true,
                    ("top", "bottom") | ("bottom", "top") => false,
                    _ => {
                        return Err(
                            "a split needs `left` and `right`, or `top` and `bottom`".to_string()
                        )
                    }
                };

                let first_dominant = dominant == "left" || dominant == "top";
                let (first, second) = if first_dominant { (a, b) } else { (b, a) };
                Ok(Self::Split(Box::new(Split {
                    vertical,
                    first_dominant,
                    scale,
                    first,
                    second,
                })))
            }
            _ => Err("a layout must be a container name or a table".to_string()),
        }
    }

    /// A one-line summary for the layout menu, like `single 60% | stack`: `|` splits left/right,
    /// `/` splits top/bottom, and the dominant side carries its share of the space.
    pub fn describe(&self) -> String {
        match self {
            Self::Container { views, kind } => {
                let name = format!("{kind:?}").to_lowercase();
                if *views == DEFAULT_VIEWS {
                    name
                } else {
                    format!("{name}({views})")
                }
            }
            Self::Split(split) => {
                let side = |layout: &Layout, dominant: bool| {
                    let text = match layout {
                        Self::Split(_) => format!("({})", layout.describe()),
                        _ => layout.describe(),
                    };
                    if !dominant {
                        return text;
                    }
                    match split.scale {
                        Scale::Fixed(scale) => format!("{text} {:.0}%", scale * 100.0),
                        Scale::Dynamic { min, max } => {
                            format!("{text} {:.0}-{:.0}%", min * 100.0, max * 100.0)
                        }
                    }
                };
                let separator = if split.vertical { "|" } else { "/" };
                format!(
                    "{} {separator} {}",
                    side(&split.first, split.first_dominant),
                    side(&split.second, !split.first_dominant)
                )
            }
        }
    }

    fn capacity(&self) -> usize {
        match self {
            Self::Split(split) => split
                .first
                .capacity()
                .saturating_add(split.second.capacity()),
            Self::Container {
                kind: Container::Empty,
                ..
            } => 0,
            Self::Container {
                kind: Container::Single,
                views,
            } => (*views).min(1),
            Self::Container { views, .. } => *views,
        }
    }

    /// Computes a tile for each view, in layout order. `sizes` are the views' current sizes,
    /// used by dynamic scaling. Views beyond the layout's capacity get no tile.
    pub fn arrange(
        &self,
        area: Rectangle<i32, Logical>,
        sizes: &[Size<i32, Logical>],
    ) -> Vec<Rectangle<i32, Logical>> {
        let n = sizes.len().min(self.capacity());
        if n == 0 {
            return Vec::new();
        }

        match self {
            Self::Container { kind, .. } => kind.arrange(area, n),
            Self::Split(split) => split.arrange(area, &sizes[..n]),
        }
    }
}

impl Split {
    fn arrange(
        &self,
        area: Rectangle<i32, Logical>,
        sizes: &[Size<i32, Logical>],
    ) -> Vec<Rectangle<i32, Logical>> {
        let (dominant, other) = if self.first_dominant {
            (&self.first, &self.second)
        } else {
            (&self.second, &self.first)
        };

        let n_dominant = sizes.len().min(dominant.capacity());
        let n_other = (sizes.len() - n_dominant).min(other.capacity());

        // If the other side could take views but has none, the dominant side gets everything.
        if n_other == 0 && other.capacity() > 0 {
            return dominant.arrange(area, sizes);
        }

        let total = if self.vertical {
            area.size.w
        } else {
            area.size.h
        };
        let ratio = match self.scale {
            Scale::Fixed(scale) => scale,
            Scale::Dynamic { min, max } => {
                let first = sizes[0];
                let size = if self.vertical { first.w } else { first.h };
                (size as f64 / total.max(1) as f64).clamp(min, max)
            }
        };
        let dominant_len = (total as f64 * ratio).round() as i32;
        let first_len = if self.first_dominant {
            dominant_len
        } else {
            total - dominant_len
        };

        let (first_area, second_area) = split_rect(area, self.vertical, first_len);
        let (dominant_area, other_area) = if self.first_dominant {
            (first_area, second_area)
        } else {
            (second_area, first_area)
        };

        let mut tiles = dominant.arrange(dominant_area, &sizes[..n_dominant]);
        tiles.extend(other.arrange(other_area, &sizes[n_dominant..n_dominant + n_other]));
        tiles
    }
}

impl Scale {
    fn parse(value: &toml::Value) -> Result<Self, String> {
        let bound = |v: &toml::Value| -> Result<f64, String> {
            let f = v.as_float().ok_or("scale values must be floats like 0.5")?;
            Ok(f.clamp(0.1, 0.9))
        };
        match value {
            toml::Value::Table(table) => {
                let min = table.get("min").map(bound).transpose()?.unwrap_or(0.1);
                let max = table.get("max").map(bound).transpose()?.unwrap_or(0.9);
                Ok(Self::Dynamic { min, max })
            }
            value => Ok(Self::Fixed(bound(value)?)),
        }
    }
}

impl Container {
    fn parse(name: &str) -> Result<Self, String> {
        Ok(match name {
            "empty" => Self::Empty,
            "single" => Self::Single,
            "full" => Self::Full,
            "stack" => Self::Stack,
            "queue" => Self::Queue,
            "grid" => Self::Grid,
            _ => return Err(format!("unknown layout `{name}`")),
        })
    }

    fn arrange(self, area: Rectangle<i32, Logical>, n: usize) -> Vec<Rectangle<i32, Logical>> {
        match self {
            Self::Empty => Vec::new(),
            Self::Single => vec![area],
            Self::Full => vec![area; n],
            Self::Stack => divide(area.size.h, n)
                .map(|(y, h)| {
                    Rectangle::new(area.loc + Point::from((0, y)), (area.size.w, h).into())
                })
                .collect(),
            Self::Queue => divide(area.size.w, n)
                .map(|(x, w)| {
                    Rectangle::new(area.loc + Point::from((x, 0)), (w, area.size.h).into())
                })
                .collect(),
            Self::Grid => {
                let cols = (n as f64).sqrt().ceil() as usize;
                let rows = n.div_ceil(cols);
                divide(area.size.h, rows)
                    .flat_map(|(y, h)| {
                        divide(area.size.w, cols).map(move |(x, w)| {
                            Rectangle::new(area.loc + Point::from((x, y)), (w, h).into())
                        })
                    })
                    .take(n)
                    .collect()
            }
        }
    }
}

/// Splits `len` into `n` (offset, length) pieces; leftover pixels go to the first piece.
fn divide(len: i32, n: usize) -> impl Iterator<Item = (i32, i32)> {
    let n = n.max(1) as i32;
    let base = len / n;
    let extra = len % n;
    (0..n).map(move |i| {
        let offset = if i == 0 { 0 } else { extra + i * base };
        let size = if i == 0 { base + extra } else { base };
        (offset, size)
    })
}

fn split_rect(
    area: Rectangle<i32, Logical>,
    vertical: bool,
    first_len: i32,
) -> (Rectangle<i32, Logical>, Rectangle<i32, Logical>) {
    if vertical {
        let first = Rectangle::new(area.loc, (first_len, area.size.h).into());
        let second = Rectangle::new(
            area.loc + Point::from((first_len, 0)),
            (area.size.w - first_len, area.size.h).into(),
        );
        (first, second)
    } else {
        let first = Rectangle::new(area.loc, (area.size.w, first_len).into());
        let second = Rectangle::new(
            area.loc + Point::from((0, first_len)),
            (area.size.w, area.size.h - first_len).into(),
        );
        (first, second)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(src: &str) -> Layout {
        let table: toml::Table = toml::from_str(&format!("l = {src}")).unwrap();
        Layout::parse(&table["l"]).unwrap()
    }

    fn area() -> Rectangle<i32, Logical> {
        Rectangle::new((0, 0).into(), (1000, 600).into())
    }

    fn sizes(n: usize) -> Vec<Size<i32, Logical>> {
        vec![(100, 100).into(); n]
    }

    #[test]
    fn describe() {
        let describe = |src| layout(src).describe();
        assert_eq!(describe(r#""grid""#), "grid");
        assert_eq!(
            describe(r#"{ left = "single", right = "stack", scale = 0.6 }"#),
            "single 60% | stack"
        );
        assert_eq!(
            describe(r#"{ right = "single", left = "stack", scale = 0.7 }"#),
            "stack | single 70%"
        );
        assert_eq!(
            describe(
                r#"{ top = "single", bottom = { views = 4, layout = "queue" }, scale = 0.7 }"#
            ),
            "single 70% / queue(4)"
        );
        assert_eq!(
            describe(r#"{ left = "single", right = { top = "full", bottom = "grid" } }"#),
            "single 50% | (full 50% / grid)"
        );
    }

    #[test]
    fn main_and_stack() {
        let tiles = layout(r#"{ left = "single", right = "stack", scale = 0.6 }"#)
            .arrange(area(), &sizes(3));
        assert_eq!(tiles[0], Rectangle::new((0, 0).into(), (600, 600).into()));
        assert_eq!(tiles[1], Rectangle::new((600, 0).into(), (400, 300).into()));
        assert_eq!(
            tiles[2],
            Rectangle::new((600, 300).into(), (400, 300).into())
        );
    }

    #[test]
    fn dominant_side_takes_everything_when_alone() {
        let tiles = layout(r#"{ left = "single", right = "stack" }"#).arrange(area(), &sizes(1));
        assert_eq!(tiles, vec![area()]);
    }

    #[test]
    fn right_side_dominant() {
        let tiles = layout(r#"{ right = "single", left = "stack", scale = 0.7 }"#)
            .arrange(area(), &sizes(2));
        assert_eq!(tiles[0], Rectangle::new((300, 0).into(), (700, 600).into()));
        assert_eq!(tiles[1], Rectangle::new((0, 0).into(), (300, 600).into()));
    }

    #[test]
    fn empty_keeps_its_space() {
        let tiles = layout(r#"{ left = "empty", right = "single" }"#).arrange(area(), &sizes(2));
        assert_eq!(
            tiles,
            vec![Rectangle::new((500, 0).into(), (500, 600).into())]
        );
    }

    #[test]
    fn capped_container_and_grid() {
        let tiles = layout(r#"{ views = 2, layout = "queue" }"#).arrange(area(), &sizes(5));
        assert_eq!(tiles.len(), 2);

        let tiles = layout(r#""grid""#).arrange(area(), &sizes(5));
        assert_eq!(tiles.len(), 5);
        assert_eq!(tiles[3], Rectangle::new((0, 300).into(), (334, 300).into()));
    }
}

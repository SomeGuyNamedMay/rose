//! Builds the frame: the menu and bar, views in stacking order with their borders, the client's
//! cursor and the wallpaper.

use smithay::{
    backend::renderer::{
        element::{
            memory::MemoryRenderBufferRenderElement,
            solid::SolidColorRenderElement,
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            AsRenderElements, Kind,
        },
        gles::GlesRenderer,
    },
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    utils::{Logical, Rectangle, Scale},
    wayland::compositor::with_states,
};

use crate::Rose;

smithay::backend::renderer::element::render_elements! {
    pub RoseRenderElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Solid=SolidColorRenderElement,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
}

impl Rose {
    /// Render elements for the output, front to back.
    pub fn render_elements(&mut self, renderer: &mut GlesRenderer) -> Vec<RoseRenderElement> {
        let scale = Scale::from(1.0);
        let mut elements: Vec<RoseRenderElement> = Vec::new();

        if let CursorImageStatus::Surface(surface) = &self.cursor_status {
            let hotspot = with_states(surface, |states| {
                states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .map(|data| data.lock().unwrap().hotspot)
                    .unwrap_or_default()
            });
            let location = self
                .seat
                .get_pointer()
                .unwrap()
                .current_location()
                .to_i32_round()
                - hotspot;
            elements.extend(render_elements_from_surface_tree(
                renderer,
                surface,
                location.to_physical(1),
                scale,
                1.0,
                Kind::Cursor,
            ));
        }

        elements.extend(self.panel_elements(renderer));

        let border = self.config.ui.border;
        let windows: Vec<_> = self.space.elements().rev().cloned().collect();
        for window in windows {
            let Some(location) = self.space.element_location(&window) else {
                continue;
            };
            let geometry = window.geometry();
            let render_location = (location - geometry.loc).to_physical(1);
            elements.extend(window.render_elements::<RoseRenderElement>(
                renderer,
                render_location,
                scale,
                1.0,
            ));

            if border <= 0 || geometry.size.is_empty() {
                continue;
            }
            let color = self.border_color(&window).0;
            let rect = Rectangle::new(location, geometry.size);
            let Some(view) = self.view_mut(&window) else {
                continue;
            };
            for (buffer, edge) in view.borders.iter_mut().zip(border_rects(rect, border)) {
                buffer.update(edge.size, color);
                elements.push(
                    SolidColorRenderElement::from_buffer(
                        buffer,
                        edge.loc.to_physical(1),
                        scale,
                        1.0,
                        Kind::Unspecified,
                    )
                    .into(),
                );
            }
        }

        elements.extend(self.wallpaper_element(renderer));
        elements
    }
}

/// Top, bottom, left and right border rectangles around `rect`.
fn border_rects(rect: Rectangle<i32, Logical>, width: i32) -> [Rectangle<i32, Logical>; 4] {
    let (x, y, w, h) = (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h);
    [
        Rectangle::new((x - width, y - width).into(), (w + 2 * width, width).into()),
        Rectangle::new((x - width, y + h).into(), (w + 2 * width, width).into()),
        Rectangle::new((x - width, y).into(), (width, h).into()),
        Rectangle::new((x + w, y).into(), (width, h).into()),
    ]
}

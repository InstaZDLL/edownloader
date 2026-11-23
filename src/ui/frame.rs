//! The window's edges, where the app draws its own frame.
//!
//! On Linux the app asks for client-side decorations (main.rs): no system
//! title bar, no system border. The title bar is the app's own; this module
//! gives the window back what the system border did: a few pixels along each
//! edge that resize it, with the matching cursor.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, point, px, AnyElement, Bounds, CursorStyle, Decorations, DispatchPhase, HitboxBehavior,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, ResizeEdge, Size,
    Styled, Window,
};

/// Width of the band along each edge that resizes the window.
const EDGE: Pixels = px(5.);

/// An overlay over the whole window, to be its last child: present only when
/// the window draws its own frame and is not maximized or tiled.
pub fn resize_edges(window: &Window) -> Option<AnyElement> {
    let Decorations::Client { tiling } = window.window_decorations() else {
        return None;
    };
    if window.is_maximized() || window.is_fullscreen() || tiling.is_tiled() {
        return None;
    }
    // Repaint only when the pointer crosses into or out of the band, so the
    // cursor follows without redrawing the app on every move.
    let on_edge = Rc::new(Cell::new(false));
    Some(
        canvas(
            |_, window, _| {
                window.insert_hitbox(
                    Bounds::new(point(px(0.), px(0.)), window_size(window)),
                    HitboxBehavior::Normal,
                )
            },
            move |_, hitbox, window, _| {
                let size = window_size(window);
                if let Some(edge) = edge_at(window.mouse_position(), size) {
                    window.set_cursor_style(cursor_for(edge), &hitbox);
                }
                // Captured before the app's own handlers: a press on the
                // band resizes, even over the title bar's drag area.
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                        return;
                    }
                    if let Some(edge) = edge_at(event.position, window_size(window)) {
                        window.start_window_resize(edge);
                        cx.stop_propagation();
                    }
                });
                let on_edge = on_edge.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let now = edge_at(event.position, window_size(window)).is_some();
                    if on_edge.replace(now) != now {
                        window.refresh();
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element(),
    )
}

fn window_size(window: &Window) -> Size<Pixels> {
    window.window_bounds().get_bounds().size
}

fn cursor_for(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

/// Which edge or corner `pos` is on, if any. Corners are a little larger than
/// the band, so they can be found without pixel hunting.
fn edge_at(pos: Point<Pixels>, size: Size<Pixels>) -> Option<ResizeEdge> {
    let corner = EDGE * 3.;
    let top = pos.y < EDGE;
    let bottom = pos.y > size.height - EDGE;
    let left = pos.x < EDGE;
    let right = pos.x > size.width - EDGE;
    let near_top = pos.y < corner;
    let near_bottom = pos.y > size.height - corner;
    let near_left = pos.x < corner;
    let near_right = pos.x > size.width - corner;
    Some(if (top && near_left) || (left && near_top) {
        ResizeEdge::TopLeft
    } else if (top && near_right) || (right && near_top) {
        ResizeEdge::TopRight
    } else if (bottom && near_left) || (left && near_bottom) {
        ResizeEdge::BottomLeft
    } else if (bottom && near_right) || (right && near_bottom) {
        ResizeEdge::BottomRight
    } else if top {
        ResizeEdge::Top
    } else if bottom {
        ResizeEdge::Bottom
    } else if left {
        ResizeEdge::Left
    } else if right {
        ResizeEdge::Right
    } else {
        return None;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    #[test]
    fn edges_and_corners_are_found() {
        let window = size(px(800.), px(600.));
        assert_eq!(edge_at(point(px(400.), px(300.)), window), None);
        assert_eq!(
            edge_at(point(px(400.), px(1.)), window),
            Some(ResizeEdge::Top)
        );
        assert_eq!(
            edge_at(point(px(799.), px(300.)), window),
            Some(ResizeEdge::Right)
        );
        assert_eq!(
            edge_at(point(px(2.), px(8.)), window),
            Some(ResizeEdge::TopLeft)
        );
        assert_eq!(
            edge_at(point(px(790.), px(598.)), window),
            Some(ResizeEdge::BottomRight)
        );
    }
}

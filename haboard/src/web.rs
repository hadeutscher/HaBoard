//! Browser helpers for embedding a [`Scene`](crate::Scene) in a page.
//!
//! Available on `wasm32` targets, and independent of the `winit` feature.
//!
//! These are the parts of canvas wiring that are fiddly but not a matter of
//! taste: converting a pointer event into the scene's physical-pixel
//! coordinates, mapping a `KeyboardEvent` onto [`Key`], and sizing a canvas's
//! backing store. They are free functions rather than a component, because
//! everything *around* them is a policy an embedder should be free to choose —
//! where listeners attach, whether the canvas is focusable, whether a pen
//! counts as a mouse or a finger. See `examples/canvas-embed` for one set of
//! answers.

use crate::input::{Key, Modifiers};

/// The device pixel ratio, or `1.0` if there is no window.
///
/// This is not a constant: it changes with browser zoom as well as with the
/// display, so anything derived from it — a backing store size, a rasterised
/// label — has to be revisited rather than computed once at startup.
pub fn device_pixel_ratio() -> f64 {
    web_sys::window().map_or(1.0, |w| w.device_pixel_ratio())
}

/// The canvas's CSS box converted to physical pixels.
///
/// This is the size to give both the canvas's backing store (`width`/`height`)
/// and the haboard surface, which works entirely in physical pixels. Never
/// smaller than 1×1, since a surface cannot be configured with a zero extent.
pub fn canvas_physical_size(canvas: &web_sys::HtmlCanvasElement) -> (u32, u32) {
    let ratio = device_pixel_ratio();
    let rect = canvas.get_bounding_client_rect();
    let width = (rect.width() * ratio).round().max(1.0) as u32;
    let height = (rect.height() * ratio).round().max(1.0) as u32;
    (width, height)
}

/// Size a canvas's backing store to its CSS box, returning that size.
///
/// `current` is the size the surface is at **now**, not the size you want —
/// pass [`Scene::size`](crate::Scene::size) or [`Engine::size`](crate::Engine::size),
/// or `(0, 0)` if there is no surface yet. Passing the desired size instead
/// compares equal every time, so the store is never resized and the failure
/// looks like a working program.
///
/// Returns `None` if the size is unchanged, so a caller can skip the
/// reconfiguration — assigning `width`/`height` clears the canvas even when the
/// value is identical.
///
/// ```no_run
/// # use haboard::{Drawable, Input, Scene, web};
/// # fn f<T: Drawable>(scene: &mut Scene<T>, canvas: &web_sys::HtmlCanvasElement) {
/// if let Some((width, height)) = web::resize_canvas_backing_store(canvas, scene.size()) {
///     scene.handle(Input::Resize { width, height });
/// }
/// # }
/// ```
pub fn resize_canvas_backing_store(
    canvas: &web_sys::HtmlCanvasElement,
    current: (u32, u32),
) -> Option<(u32, u32)> {
    let size = canvas_physical_size(canvas);
    if size == current {
        return None;
    }
    canvas.set_width(size.0);
    canvas.set_height(size.1);
    Some(size)
}

/// Convert a pointer event's viewport coordinates into physical pixels relative
/// to the canvas's top-left corner.
pub fn pointer_position(
    event: &web_sys::PointerEvent,
    canvas: &web_sys::HtmlCanvasElement,
) -> (f32, f32) {
    let ratio = device_pixel_ratio();
    let rect = canvas.get_bounding_client_rect();
    let x = (f64::from(event.client_x()) - rect.left()) * ratio;
    let y = (f64::from(event.client_y()) - rect.top()) * ratio;
    (x as f32, y as f32)
}

/// Map a `KeyboardEvent`'s `key` onto haboard's [`Key`].
///
/// Anything the scene has no behaviour for becomes [`Key::Unidentified`].
pub fn key_from_event(event: &web_sys::KeyboardEvent) -> Key {
    key_from_name(&event.key())
}

/// Map a DOM `KeyboardEvent.key` string onto haboard's [`Key`].
pub fn key_from_name(key: &str) -> Key {
    match key {
        "Escape" => Key::Escape,
        "Delete" => Key::Delete,
        "Backspace" => Key::Backspace,
        "ArrowLeft" => Key::ArrowLeft,
        "ArrowRight" => Key::ArrowRight,
        "ArrowUp" => Key::ArrowUp,
        "ArrowDown" => Key::ArrowDown,
        // A printable key reports its character here; anything longer is a
        // named key haboard has no behaviour for.
        other => {
            let mut chars = other.chars().flat_map(char::to_lowercase);
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Character(c),
                _ => Key::Unidentified,
            }
        }
    }
}

/// Read modifier state off a `KeyboardEvent`.
pub fn modifiers_from_keyboard_event(event: &web_sys::KeyboardEvent) -> Modifiers {
    Modifiers {
        ctrl: event.ctrl_key(),
        shift: event.shift_key(),
        alt: event.alt_key(),
        meta: event.meta_key(),
    }
}

/// Read modifier state off a `PointerEvent`.
pub fn modifiers_from_pointer_event(event: &web_sys::PointerEvent) -> Modifiers {
    Modifiers {
        ctrl: event.ctrl_key(),
        shift: event.shift_key(),
        alt: event.alt_key(),
        meta: event.meta_key(),
    }
}

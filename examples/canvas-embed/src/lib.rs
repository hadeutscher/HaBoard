//! haboard embedded in a page that owns its own event loop.
//!
//! No winit: haboard is built with `default-features = false`, so the canvas
//! goes straight to wgpu and browser events are translated into
//! [`haboard::Input`] here. This is the case a host application is in — a
//! framework already owns the page, the element and the frame loop, and the
//! board is one control inside it.
//!
//! Built with [Trunk](https://trunkrs.dev): `trunk serve --release`.
//!
//! The four things an embedder has to do, all visible below:
//!
//! 1. Size the canvas backing store (CSS size × `devicePixelRatio`) and keep it
//!    in sync with a `ResizeObserver`. haboard works in physical pixels and
//!    applies no scale factor of its own.
//! 2. Translate pointer and key events into [`Input`].
//! 3. Drive frames, skipping when [`Scene::needs_redraw`] says nothing changed.
//! 4. Persist when a [`Response`] reports [`Commit::Now`].

mod token;

use std::{cell::RefCell, rc::Rc};

use haboard::{
    Commit, DrawableId, Engine, Input, PointerId, PointerPhase, Scene, SceneMode,
    web::{
        canvas_physical_size, device_pixel_ratio, key_from_event, modifiers_from_keyboard_event,
        modifiers_from_pointer_event, pointer_position, resize_canvas_backing_store,
    },
};
use token::Token;
use wasm_bindgen::{JsCast, prelude::*};

/// Everything the event callbacks need. `Rc<RefCell<_>>` rather than a channel
/// or a thread-local side-channel: the host owns the scene outright, so
/// callbacks can just borrow it.
struct Board {
    scene: Scene<Token>,
    canvas: web_sys::HtmlCanvasElement,
    /// Next hue to hand out, so added tokens are visually distinguishable.
    next_hue: u32,
    /// Counter behind the placeholder names given to new tokens.
    next_index: u32,
    /// The device pixel ratio the textures were last rasterised at. Browser
    /// zoom changes this without changing the CSS box, so it has to be tracked
    /// separately from the size.
    texture_ratio: f64,
}

type Shared = Rc<RefCell<Board>>;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    wasm_bindgen_futures::spawn_local(async {
        if let Err(e) = run().await {
            log::error!("could not start the board: {e:?}");
            set_status(&format!("Could not start the board: {e:?}"));
        }
    });
}

fn document() -> web_sys::Document {
    web_sys::window()
        .expect("no window")
        .document()
        .expect("no document")
}

fn element(id: &str) -> Option<web_sys::HtmlElement> {
    document()
        .get_element_by_id(id)?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()
}

fn set_status(text: &str) {
    if let Some(status) = element("status") {
        status.set_inner_text(text);
    }
}

async fn run() -> Result<(), JsValue> {
    let canvas = document()
        .get_element_by_id("board")
        .ok_or("no #board element")?
        .dyn_into::<web_sys::HtmlCanvasElement>()?;

    // Keyboard events go to the focused element. Making the canvas focusable —
    // rather than listening on the window — is what stops the board from
    // stealing arrow keys and Delete from the rest of the page.
    canvas.set_attribute("tabindex", "0")?;

    let size = canvas_physical_size(&canvas);
    canvas.set_width(size.0);
    canvas.set_height(size.1);

    // This is the whole of the GPU setup. `Engine::new` is async because
    // requesting an adapter and a device are futures on the web, so a host
    // always has a brief not-ready state — here it is simply the time before
    // this function returns.
    let engine = Engine::from_canvas(canvas.clone(), size)
        .await
        .map_err(|e| JsValue::from_str(&format!("{e}")))?;

    let mut scene = Scene::new(engine, starting_tokens(), SceneMode::Edit);
    scene.engine_mut().clear_color = wgpu_clear_colour();

    let board = Rc::new(RefCell::new(Board {
        scene,
        canvas: canvas.clone(),
        next_hue: 250,
        next_index: 4,
        texture_ratio: device_pixel_ratio(),
    }));

    wire_pointer_events(&board)?;
    wire_keyboard_events(&board)?;
    wire_resize_observer(&board)?;
    wire_controls(&board)?;
    start_frame_loop(&board);

    refresh_status(&board.borrow());
    Ok(())
}

fn wgpu_clear_colour() -> haboard::wgpu::Color {
    haboard::wgpu::Color {
        r: 0.12,
        g: 0.13,
        b: 0.16,
        a: 1.0,
    }
}

fn starting_tokens() -> Vec<Token> {
    vec![
        Token::new("Avital", 80.0, 80.0, 96.0, 10),
        Token::new("Boaz", 240.0, 140.0, 96.0, 90),
        Token::new("Chen", 400.0, 90.0, 96.0, 170),
        // A right-to-left name, because the rasteriser must not assume
        // otherwise and the initial is taken from the logical start of the
        // string rather than its visual left edge.
        Token::new("יעל", 180.0, 300.0, 96.0, 320),
    ]
}

// ---------------------------------------------------------------------------
// Event translation
// ---------------------------------------------------------------------------

/// Which [`PointerId`] a DOM pointer event maps to.
///
/// This is the example's policy, not haboard's, which is why it lives here
/// rather than in `haboard::web`: treating a pen as a finger suits a seating
/// chart, but a drawing tool would want the opposite, and neither answer
/// belongs in the library.
fn pointer_id_of(event: &web_sys::PointerEvent) -> PointerId {
    if event.pointer_type() == "mouse" {
        PointerId::Mouse
    } else {
        PointerId::Touch(event.pointer_id() as u64)
    }
}

/// Register a DOM listener, leaking the closure so it outlives this call.
/// A real host would keep the [`Closure`] and drop it on unmount.
fn listen<E: wasm_bindgen::convert::FromWasmAbi + 'static>(
    target: &web_sys::EventTarget,
    event: &str,
    handler: impl FnMut(E) + 'static,
) -> Result<(), JsValue> {
    let closure = Closure::<dyn FnMut(E)>::new(handler);
    target.add_event_listener_with_callback(event, closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn wire_pointer_events(board: &Shared) -> Result<(), JsValue> {
    let canvas = board.borrow().canvas.clone();

    for (name, phase) in [
        ("pointerdown", PointerPhase::Down),
        ("pointermove", PointerPhase::Move),
        ("pointerup", PointerPhase::Up),
        ("pointercancel", PointerPhase::Cancel),
    ] {
        let board = Rc::clone(board);
        let canvas_for_handler = canvas.clone();
        listen(
            canvas.as_ref(),
            name,
            move |event: web_sys::PointerEvent| {
                let mut b = board.borrow_mut();
                if phase == PointerPhase::Down {
                    // Capture so a drag keeps delivering events after the pointer
                    // leaves the canvas, and focus so the keyboard shortcuts work.
                    let _ = canvas_for_handler.set_pointer_capture(event.pointer_id());
                    let _ = canvas_for_handler.focus();
                }
                let (x, y) = pointer_position(&event, &canvas_for_handler);
                b.scene
                    .handle(Input::Modifiers(modifiers_from_pointer_event(&event)));
                let response = b.scene.handle(Input::Pointer {
                    id: pointer_id_of(&event),
                    phase,
                    x,
                    y,
                });
                if response.handled {
                    event.prevent_default();
                }
                after_input(&mut b, response.commit, response.selection_changed);
            },
        )?;
    }
    Ok(())
}

fn wire_keyboard_events(board: &Shared) -> Result<(), JsValue> {
    let canvas = board.borrow().canvas.clone();

    for (name, pressed) in [("keydown", true), ("keyup", false)] {
        let board = Rc::clone(board);
        listen(
            canvas.as_ref(),
            name,
            move |event: web_sys::KeyboardEvent| {
                let mut b = board.borrow_mut();
                b.scene
                    .handle(Input::Modifiers(modifiers_from_keyboard_event(&event)));
                let response = b.scene.handle(Input::Key {
                    key: key_from_event(&event),
                    pressed,
                    repeat: event.repeat(),
                });
                if response.handled {
                    // Stops arrow keys scrolling the page and Backspace navigating.
                    event.prevent_default();
                }
                after_input(&mut b, response.commit, response.selection_changed);
            },
        )?;
    }
    Ok(())
}

/// Keep the backing store — and the rasterised textures — in step with the
/// canvas's real size in device pixels.
///
/// Two details matter here, and getting either wrong is silent:
///
/// A `ResizeObserver` rather than a window `resize` listener, because the
/// canvas is one element in a host layout and can change size without the
/// window doing so.
///
/// And `device-pixel-content-box` rather than the default `content-box`,
/// because browser zoom changes `devicePixelRatio` while leaving the CSS box
/// identical. A `content-box` observer never fires for that, so the surface
/// keeps its old resolution and everything drawn at the previous ratio — text
/// especially — stays blurry until the page is reloaded.
fn wire_resize_observer(board: &Shared) -> Result<(), JsValue> {
    let canvas = board.borrow().canvas.clone();
    let board = Rc::clone(board);
    let callback = Closure::<dyn FnMut(js_sys::Array)>::new(move |_: js_sys::Array| {
        let mut b = board.borrow_mut();

        let current = b.scene.size();
        let canvas = b.canvas.clone();
        if let Some(size) = resize_canvas_backing_store(&canvas, current) {
            b.scene.resize(size);
        }

        // A ratio change means every host-rasterised texture was drawn for the
        // wrong resolution. The geometry is in physical pixels and the resize
        // above has already handled it; this re-uploads the pixels.
        let ratio = device_pixel_ratio();
        if (ratio - b.texture_ratio).abs() > f64::EPSILON {
            b.texture_ratio = ratio;
            let ids: Vec<DrawableId> = b.scene.drawables.ids().collect();
            for id in ids {
                b.scene.drawables.refresh_image(id);
            }
            b.scene.request_redraw();
        }
    });
    let observer = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref())?;
    let options = web_sys::ResizeObserverOptions::new();
    options.set_box(web_sys::ResizeObserverBoxOptions::DevicePixelContentBox);
    observer.observe_with_options(canvas.as_ref(), &options);
    callback.forget();
    // The observer must outlive this function; a host that can unmount would
    // keep it and call `disconnect()` instead.
    std::mem::forget(observer);
    Ok(())
}

// ---------------------------------------------------------------------------
// Frame loop
// ---------------------------------------------------------------------------

/// Render on every animation frame, but only when something changed.
///
/// The host owns the cadence: haboard never asks for a frame. `needs_redraw` is
/// advisory, so rendering unconditionally would also be correct — just wasteful.
fn start_frame_loop(board: &Shared) {
    let board = Rc::clone(board);
    let callback = Rc::new(RefCell::new(None::<Closure<dyn FnMut()>>));
    let next = Rc::clone(&callback);
    *callback.borrow_mut() = Some(Closure::<dyn FnMut()>::new(move || {
        {
            let mut b = board.borrow_mut();
            if b.scene.needs_redraw() {
                b.scene.render();
            }
        }
        if let Some(cb) = next.borrow().as_ref() {
            request_animation_frame(cb);
        }
    }));
    if let Some(cb) = callback.borrow().as_ref() {
        request_animation_frame(cb);
    }
}

fn request_animation_frame(callback: &Closure<dyn FnMut()>) {
    if let Some(window) = web_sys::window() {
        let _ = window.request_animation_frame(callback.as_ref().unchecked_ref());
    }
}

// ---------------------------------------------------------------------------
// Host reactions
// ---------------------------------------------------------------------------

/// React to a [`Response`](haboard::Response).
///
/// `Commit::Defer` marks a change that is still in progress — a held arrow key
/// — so it deliberately does not save. The release that ends the run reports
/// `Commit::Now` and flushes everything the repeats accumulated.
fn after_input(board: &mut Board, commit: Commit, selection_changed: bool) {
    if commit.is_now() {
        save(board);
    }
    if selection_changed || commit.is_change() {
        refresh_status(board);
    }
}

fn save(board: &Board) {
    // A real host would persist here. The arrangement is read straight off the
    // scene: ids are haboard's, but the payload is the host's own data.
    log::info!(
        "saving {} tokens: {:?}",
        board.scene.drawables.count(),
        board
            .scene
            .drawables
            .iter()
            .map(|t| (t.name.as_str(), t.x, t.y))
            .collect::<Vec<_>>()
    );
}

fn refresh_status(board: &Board) {
    let total = board.scene.drawables.count();
    let selected: Vec<&str> = board
        .scene
        .drawables
        .iter_with_ids()
        .filter(|(id, _)| board.scene.drawables.is_selected(*id))
        .map(|(_, token)| token.name.as_str())
        .collect();
    let status = if selected.is_empty() {
        format!("{total} tokens. Click to select, drag to move, Ctrl+drag to snap.")
    } else {
        format!("{total} tokens. Selected: {}.", selected.join(", "))
    };
    set_status(&status);
}

fn selected_ids(board: &Board) -> Vec<DrawableId> {
    board.scene.drawables.selected_ids().collect()
}

// ---------------------------------------------------------------------------
// Host controls
// ---------------------------------------------------------------------------

fn on_click(
    board: &Shared,
    id: &'static str,
    mut action: impl FnMut(&mut Board) + 'static,
) -> Result<(), JsValue> {
    let Some(button) = element(id) else {
        return Ok(());
    };
    let board = Rc::clone(board);
    listen(button.as_ref(), "click", move |_: web_sys::Event| {
        let mut b = board.borrow_mut();
        action(&mut b);
        refresh_status(&b);
    })
}

fn wire_controls(board: &Shared) -> Result<(), JsValue> {
    on_click(board, "add", |b| {
        let hue = b.next_hue;
        let index = b.next_index;
        b.next_hue = (b.next_hue + 67) % 360;
        b.next_index += 1;
        let offset = 40.0 + (index % 6) as f32 * 30.0;
        let token = Token::new(format!("Guest {index}"), offset, offset, 96.0, hue);
        // Through the scene rather than through `drawables`, so Ctrl+Z undoes it.
        b.scene.add_drawable(token);
        save(b);
    })?;

    // The reason `refresh_image` exists: the texture is derived from host data,
    // so changing that data leaves the GPU copy stale.
    on_click(board, "rename", |b| {
        let ids = selected_ids(b);
        if ids.is_empty() {
            set_status("Select a token first, then rename it.");
            return;
        }
        for id in ids {
            if let Some(token) = b.scene.drawables.get_mut(id) {
                let renamed = format!("Zed {}", token.hue);
                token.rename(renamed);
            }
            b.scene.drawables.refresh_image(id);
        }
        // Mutating through `drawables` is invisible to the scene, so the redraw
        // has to be requested explicitly.
        b.scene.request_redraw();
        save(b);
    })?;

    on_click(board, "delete", |b| {
        let ids = selected_ids(b);
        if ids.is_empty() {
            set_status("Select a token first, then delete it.");
            return;
        }
        for id in ids {
            // Undoable, and safe with a drag or history outstanding: the scene
            // refers to entries by id, not by position.
            b.scene.remove_drawable(id);
        }
        save(b);
    })?;

    on_click(board, "undo", |b| {
        if b.scene.undo() {
            save(b);
        }
    })?;

    on_click(board, "redo", |b| {
        if b.scene.redo() {
            save(b);
        }
    })?;

    // The point of a surface that can be recreated: the board survives its
    // element leaving the DOM, so a host can route away from it and back
    // without rebuilding the GPU state or losing the scene.
    on_click(board, "unmount", |b| {
        let mounted = b.canvas.is_connected();
        let Some(button) = element("unmount") else {
            return;
        };
        if mounted {
            b.scene.drop_surface();
            b.canvas.remove();
            button.set_inner_text("Remount canvas");
            set_status("Canvas unmounted. The scene and every uploaded texture are still here.");
        } else {
            let Some(body) = document().body() else {
                return;
            };
            if body.append_child(b.canvas.as_ref()).is_err() {
                return;
            }
            let size = canvas_physical_size(&b.canvas);
            b.canvas.set_width(size.0);
            b.canvas.set_height(size.1);
            let canvas = b.canvas.clone();
            match b.scene.recreate_surface_from_canvas(canvas, size) {
                Ok(()) => {
                    button.set_inner_text("Unmount canvas");
                    set_status("Canvas remounted onto the same scene.");
                }
                Err(e) => set_status(&format!("Could not remount: {e}")),
            }
        }
    })?;

    Ok(())
}

//! Translation between [`winit`] events and haboard's own [`Input`].
//!
//! Available with the `winit` feature (on by default). Nothing below this layer
//! knows winit exists, so an embedder that owns its event loop can disable the
//! feature and build [`Input`] values itself.

use winit::{
    event::{ElementState, KeyEvent, MouseButton, TouchPhase, WindowEvent},
    keyboard::{ModifiersState, NamedKey},
};

use crate::{
    drawable::Drawable,
    input::{Input, Key, Modifiers, PointerId, PointerPhase, Response},
    scene::Scene,
};

/// Map a winit modifier state onto haboard's.
pub fn modifiers_from_winit(state: ModifiersState) -> Modifiers {
    Modifiers {
        ctrl: state.control_key(),
        shift: state.shift_key(),
        alt: state.alt_key(),
        meta: state.super_key(),
    }
}

/// Map a winit logical key onto haboard's.
///
/// Letters are lower-cased so that shortcut matching does not depend on shift
/// state; keys the scene has no behaviour for become [`Key::Unidentified`].
fn key_from_winit(event: &KeyEvent) -> Key {
    match &event.logical_key {
        winit::keyboard::Key::Named(NamedKey::Escape) => Key::Escape,
        winit::keyboard::Key::Named(NamedKey::Delete) => Key::Delete,
        winit::keyboard::Key::Named(NamedKey::Backspace) => Key::Backspace,
        winit::keyboard::Key::Named(NamedKey::ArrowLeft) => Key::ArrowLeft,
        winit::keyboard::Key::Named(NamedKey::ArrowRight) => Key::ArrowRight,
        winit::keyboard::Key::Named(NamedKey::ArrowUp) => Key::ArrowUp,
        winit::keyboard::Key::Named(NamedKey::ArrowDown) => Key::ArrowDown,
        winit::keyboard::Key::Character(text) => {
            let mut chars = text.chars().flat_map(char::to_lowercase);
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Character(c),
                _ => Key::Unidentified,
            }
        }
        _ => Key::Unidentified,
    }
}

/// Maps [`WindowEvent`]s onto [`Input`]s.
///
/// Stateful because winit reports a mouse press without a position — the
/// position comes from the preceding `CursorMoved`. Feed every window event to
/// [`map`](WinitInput::map) so the tracked cursor stays current.
#[derive(Debug, Clone, Copy, Default)]
pub struct WinitInput {
    cursor: (f32, f32),
}

impl WinitInput {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last cursor position winit reported, in physical pixels.
    pub fn cursor(&self) -> (f32, f32) {
        self.cursor
    }

    /// Translate one window event, returning `None` for events the scene has no
    /// vocabulary for.
    pub fn map(&mut self, event: &WindowEvent) -> Option<Input> {
        match event {
            WindowEvent::Resized(size) => Some(Input::Resize {
                width: size.width,
                height: size.height,
            }),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as f32, position.y as f32);
                Some(Input::Pointer {
                    id: PointerId::Mouse,
                    phase: PointerPhase::Move,
                    x: self.cursor.0,
                    y: self.cursor.1,
                })
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => Some(Input::Pointer {
                id: PointerId::Mouse,
                phase: match state {
                    ElementState::Pressed => PointerPhase::Down,
                    ElementState::Released => PointerPhase::Up,
                },
                x: self.cursor.0,
                y: self.cursor.1,
            }),
            // The cursor leaving mid-drag ends the gesture where it stands.
            WindowEvent::CursorLeft { .. } => Some(Input::Pointer {
                id: PointerId::Mouse,
                phase: PointerPhase::Cancel,
                x: self.cursor.0,
                y: self.cursor.1,
            }),
            WindowEvent::Touch(touch) => Some(Input::Pointer {
                id: PointerId::Touch(touch.id),
                phase: match touch.phase {
                    TouchPhase::Started => PointerPhase::Down,
                    TouchPhase::Moved => PointerPhase::Move,
                    TouchPhase::Ended => PointerPhase::Up,
                    TouchPhase::Cancelled => PointerPhase::Cancel,
                },
                x: touch.location.x as f32,
                y: touch.location.y as f32,
            }),
            WindowEvent::KeyboardInput { event, .. } => Some(Input::Key {
                key: key_from_winit(event),
                pressed: event.state == ElementState::Pressed,
                repeat: event.repeat,
            }),
            WindowEvent::ModifiersChanged(mods) => {
                Some(Input::Modifiers(modifiers_from_winit(mods.state())))
            }
            _ => None,
        }
    }
}

impl<T: Drawable> Scene<T> {
    /// Handle a winit window event.
    ///
    /// A convenience wrapper over [`Scene::handle`] for hosts already running a
    /// winit event loop. Returns the full [`Response`]; the mouse position for
    /// a click comes from the scene's own tracked cursor, so pass every window
    /// event rather than filtering beforehand.
    pub fn handle_winit_event(&mut self, event: &WindowEvent) -> Response {
        let mut mapper = WinitInput {
            cursor: self.cursor(),
        };
        match mapper.map(event) {
            Some(input) => self.handle(input),
            None => Response::IGNORED,
        }
    }

    /// Handle a winit window event, returning whether the scene acted on it.
    ///
    /// Retained for compatibility; [`handle_winit_event`](Self::handle_winit_event)
    /// reports strictly more.
    pub fn handle_window_event(&mut self, event: &WindowEvent) -> bool {
        self.handle_winit_event(event).handled
    }
}

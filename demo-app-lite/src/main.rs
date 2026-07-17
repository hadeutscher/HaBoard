//! Low-level `haboard` usage: no `SceneRunner`, no `Sprite`, no persistence.
//!
//! This demo wires `Engine` and `Scene` together inside a hand-rolled
//! `winit::application::ApplicationHandler`, and populates the scene with a
//! custom [`Object`] type — a [`haboard::Drawable`] implementation that isn't
//! `Sprite`.

mod object;

use std::sync::Arc;

use haboard::{Engine, Scene, SceneMode};
use object::Object;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

enum AppState {
    Uninitialized,
    Ready(Box<Scene<Object>>),
}

struct App {
    state: AppState,
}

fn initial_objects() -> Vec<Object> {
    vec![
        Object::new(100.0, 100.0, 120.0, [220, 60, 60]),
        Object::new(320.0, 220.0, 80.0, [60, 200, 90]),
        Object::new(540.0, 140.0, 160.0, [60, 120, 220]),
        Object::new(260.0, 420.0, 100.0, [230, 190, 40]),
    ]
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if matches!(self.state, AppState::Ready(_)) {
            return;
        }

        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes())
                .expect("failed to create window"),
        );

        let engine = pollster::block_on(Engine::new(window));
        let mut scene = Scene::new(engine, initial_objects(), SceneMode::Edit);
        scene.render();
        self.state = AppState::Ready(Box::new(scene));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                if let AppState::Ready(scene) = &mut self.state {
                    scene.render();
                }
            }
            _ => {
                if let AppState::Ready(scene) = &mut self.state {
                    scene.handle_window_event(&event);
                }
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let AppState::Ready(scene) = &self.state {
            scene.window().request_redraw();
        }
    }
}

fn main() {
    env_logger::init();

    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App {
        state: AppState::Uninitialized,
    };
    event_loop.run_app(&mut app).expect("event loop error");
}

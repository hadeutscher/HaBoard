mod scene_data;

use std::sync::Arc;

use clap::{Parser, ValueEnum};
use haboard::{Engine, Scene, SceneMode, Sprite, textures};
use scene_data::{DrawableRecord, SceneStore, TextureDef};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/// wgpu Sprite Engine demo application.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Start the engine in edit or run mode.
    #[arg(long, value_enum, default_value_t = Mode::Edit)]
    mode: Mode,
}

/// Selectable startup mode.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum Mode {
    /// Full editor: click-select, drag selected groups, rubber-band selection.
    Edit,
    /// Playback mode: no selection UI; only unlocked drawables may be dragged.
    Run,
}

impl From<Mode> for SceneMode {
    fn from(m: Mode) -> Self {
        match m {
            Mode::Edit => SceneMode::Edit,
            Mode::Run => SceneMode::Run,
        }
    }
}

// ---------------------------------------------------------------------------
// Default scene
// ---------------------------------------------------------------------------

/// Build the initial [`SceneStore`] used when no saved state is found on disk.
///
/// Each texture is rasterized to raw RGBA bytes using the pure pixel
/// generators from `haboard::textures`, so no GPU handle is required here.
fn default_store() -> SceneStore {
    use TextureDef::Rgba;
    SceneStore {
        drawables: vec![
            DrawableRecord::new(
                20.0,
                20.0,
                300.0,
                260.0,
                Rgba {
                    width: 128,
                    height: 128,
                    bytes: textures::gradient_pixels(128, 128),
                },
            ),
            DrawableRecord::new(
                360.0,
                40.0,
                192.0,
                192.0,
                Rgba {
                    width: 64,
                    height: 64,
                    bytes: textures::checkerboard_pixels(64, 64, 8),
                },
            ),
            DrawableRecord::new(
                50.0,
                420.0,
                96.0,
                96.0,
                Rgba {
                    width: 32,
                    height: 32,
                    bytes: textures::solid_pixels(32, 32, [210, 50, 50, 255]),
                },
            ),
            DrawableRecord::new(
                180.0,
                420.0,
                96.0,
                96.0,
                Rgba {
                    width: 32,
                    height: 32,
                    bytes: textures::solid_pixels(32, 32, [50, 100, 220, 255]),
                },
            ),
            DrawableRecord::new(
                120.0,
                100.0,
                144.0,
                144.0,
                Rgba {
                    width: 48,
                    height: 48,
                    bytes: textures::solid_pixels(48, 48, [40, 200, 80, 160]),
                },
            ),
            DrawableRecord::new(
                500.0,
                310.0,
                128.0,
                128.0,
                Rgba {
                    width: 128,
                    height: 128,
                    bytes: textures::circle_pixels(128, [255, 160, 20]),
                },
            ),
        ],
    }
}

// ---------------------------------------------------------------------------
// Store <-> Sprite conversion helpers
// ---------------------------------------------------------------------------

/// Build a GPU-backed [`Sprite`] from an engine-independent [`DrawableRecord`].
fn sprite_from_record(engine: &Engine, record: &DrawableRecord) -> Sprite {
    let texture = match &record.texture_def {
        TextureDef::Rgba {
            width,
            height,
            bytes,
        } => engine.create_texture_from_rgba(bytes, *width, *height),
        TextureDef::Image(bytes) => engine
            .create_texture_from_image_bytes(bytes)
            .expect("TextureDef::Image contained invalid image bytes"),
    };
    let mut sprite = Sprite::new(record.x, record.y, record.width, record.height, texture);
    sprite.locked = record.locked;
    sprite
}

/// Copy mutable state (position, size, locked) from the live scene's sprites
/// back into the store so it reflects the current layout before saving.
fn sync_store_from_scene(store: &mut SceneStore, scene: &Scene<Sprite>) {
    for (record, sprite) in store.drawables.iter_mut().zip(scene.drawables.iter()) {
        record.x = sprite.x;
        record.y = sprite.y;
        record.width = sprite.width;
        record.height = sprite.height;
        record.locked = sprite.locked;
    }
}

// ---------------------------------------------------------------------------
// Application shell
// ---------------------------------------------------------------------------

struct App {
    mode: SceneMode,
    /// Engine-independent scene state. Kept in sync with the live scene and
    /// serialized to disk when the window is closed.
    store: SceneStore,
    scene: Option<Scene<Sprite>>,
}

impl App {
    fn new(mode: SceneMode, store: SceneStore) -> Self {
        Self {
            mode,
            store,
            scene: None,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("wgpu Sprite Engine")
                        .with_inner_size(winit::dpi::LogicalSize::new(800u32, 600u32)),
                )
                .expect("Failed to create window"),
        );

        let engine = pollster::block_on(Engine::new(window));

        // Reconstruct GPU-backed sprites from the engine-independent store.
        let drawables: Vec<Sprite> = self
            .store
            .drawables
            .iter()
            .map(|r| sprite_from_record(&engine, r))
            .collect();

        self.scene = Some(Scene::new(engine, drawables, self.mode));
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        if let Some(scene) = &self.scene {
            scene.window().request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                // Flush current sprite positions back into the store, then save.
                if let Some(scene) = &self.scene {
                    sync_store_from_scene(&mut self.store, scene);
                }
                if let Err(e) = self.store.save() {
                    eprintln!("error: failed to save scene: {e}");
                }
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                if let Some(scene) = &mut self.scene {
                    scene.render();
                }
            }
            _ => {
                if let Some(scene) = &mut self.scene {
                    scene.handle_window_event(&event);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let cli = Cli::parse();

    // Try to restore the previous session; fall back to the built-in defaults.
    let store = SceneStore::load().unwrap_or_else(|| {
        println!("No saved scene found, starting with defaults.");
        default_store()
    });

    let event_loop = EventLoop::new().expect("Failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(cli.mode.into(), store);
    event_loop.run_app(&mut app).expect("Event loop error");
}

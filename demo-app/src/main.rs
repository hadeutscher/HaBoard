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

/// Longest side of a freshly-dropped image sprite, in pixels.
/// Images larger than this are scaled down uniformly; smaller ones are kept at 1:1.
const MAX_DROP_DIM: f32 = 400.0;

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
    /// Last known cursor position in physical pixels, used to place dropped images.
    cursor_pos: (f32, f32),
}

impl App {
    fn new(mode: SceneMode, store: SceneStore) -> Self {
        Self {
            mode,
            store,
            scene: None,
            cursor_pos: (0.0, 0.0),
        }
    }

    /// Handle a file dropped onto the window.
    ///
    /// Only acts in [`SceneMode::Edit`]; silently ignores the drop otherwise.
    /// Files that cannot be decoded as images are reported to stderr and skipped.
    fn handle_dropped_file(&mut self, path: &std::path::Path) {
        let in_edit = self
            .scene
            .as_ref()
            .is_some_and(|s| s.mode() == SceneMode::Edit);
        if !in_edit {
            return;
        }

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: could not read dropped file {}: {e}", path.display());
                return;
            }
        };

        // Create the GPU texture; the immutable borrow of `scene` ends at `}`.
        let texture = {
            let engine = self.scene.as_ref().unwrap().engine();
            match engine.create_texture_from_image_bytes(&bytes) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: {} is not a valid image: {e}", path.display());
                    return;
                }
            }
        };

        // Scale down so the longest side is at most MAX_DROP_DIM; keep 1:1 for smaller images.
        let img_w = texture.width as f32;
        let img_h = texture.height as f32;
        let scale = (MAX_DROP_DIM / img_w).min(MAX_DROP_DIM / img_h).min(1.0);
        let w = img_w * scale;
        let h = img_h * scale;

        // Centre the new sprite on the cursor position at the time of the drop.
        let x = (self.cursor_pos.0 - w / 2.0).max(0.0);
        let y = (self.cursor_pos.1 - h / 2.0).max(0.0);

        let sprite = Sprite::new(x, y, w, h, texture);
        let record = DrawableRecord::new(x, y, w, h, TextureDef::Image(bytes));

        self.scene.as_mut().unwrap().push_drawable(sprite);
        self.store.drawables.push(record);
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
            WindowEvent::DroppedFile(ref path) => {
                self.handle_dropped_file(path);
            }
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
                // Track cursor position so dropped files are placed under the pointer.
                if let WindowEvent::CursorMoved { position, .. } = &event {
                    self.cursor_pos = (position.x as f32, position.y as f32);
                }
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

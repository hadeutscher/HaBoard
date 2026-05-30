use std::sync::Arc;

use clap::{Parser, ValueEnum};
use haboard::{Drawables, Engine, ImageData, Scene, SceneMode, Sprite, textures};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::{Window, WindowId},
};

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

const SAVE_PATH: &str = "scene.bin";

/// Maximum display size (longest side) for a drag-dropped image, in pixels.
const MAX_DROP_DIM: f32 = 400.0;

fn save_scene(drawables: &Drawables<Sprite>) -> std::io::Result<()> {
    let sprites: Vec<Sprite> = drawables.iter().cloned().collect();
    let bytes = postcard::to_allocvec(&sprites).map_err(std::io::Error::other)?;
    std::fs::write(SAVE_PATH, bytes)
}

fn load_scene() -> Option<Vec<Sprite>> {
    let bytes = std::fs::read(SAVE_PATH).ok()?;
    match postcard::from_bytes::<Vec<Sprite>>(&bytes) {
        Ok(sprites) => Some(sprites),
        Err(e) => {
            eprintln!("warn: could not load {SAVE_PATH}: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Default scene
// ---------------------------------------------------------------------------

fn default_sprites() -> Vec<Sprite> {
    vec![
        Sprite::new(20.0, 20.0, 300.0, 260.0, textures::gradient(128, 128)),
        Sprite::new(360.0, 40.0, 192.0, 192.0, textures::checkerboard(64, 64, 8)),
        Sprite::new(
            50.0,
            420.0,
            96.0,
            96.0,
            textures::solid(32, 32, [210, 50, 50, 255]),
        ),
        Sprite::new(
            180.0,
            420.0,
            96.0,
            96.0,
            textures::solid(32, 32, [50, 100, 220, 255]),
        ),
        Sprite::new(
            120.0,
            100.0,
            144.0,
            144.0,
            textures::solid(48, 48, [40, 200, 80, 160]),
        ),
        Sprite::new(
            500.0,
            310.0,
            128.0,
            128.0,
            textures::circle(128, [255, 160, 20]),
        ),
    ]
}

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
// Application shell
// ---------------------------------------------------------------------------

struct App {
    mode: SceneMode,
    /// Initial sprites consumed once in `resumed`. `None` after first resume.
    initial_sprites: Option<Vec<Sprite>>,
    scene: Option<Scene<Sprite>>,
    /// Last known cursor position, used to place drag-dropped images.
    cursor_pos: (f32, f32),
    /// Current keyboard modifier state, used for Ctrl+S.
    modifiers: ModifiersState,
}

impl App {
    fn new(mode: SceneMode, sprites: Vec<Sprite>) -> Self {
        Self {
            mode,
            initial_sprites: Some(sprites),
            scene: None,
            cursor_pos: (0.0, 0.0),
            modifiers: ModifiersState::empty(),
        }
    }

    /// Handle a file dragged and dropped onto the window.
    ///
    /// Only acts in [`SceneMode::Edit`]. Non-image files are reported to stderr
    /// and ignored.
    fn handle_dropped_file(&mut self, path: &std::path::Path) {
        let in_edit = self
            .scene
            .as_ref()
            .is_some_and(|s| s.mode() == SceneMode::Edit);
        if !in_edit {
            return;
        }

        // Decode the image to RGBA to get its dimensions and pixel data.
        let raw_bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: could not read {}: {e}", path.display());
                return;
            }
        };
        let img = match image::load_from_memory(&raw_bytes) {
            Ok(img) => img.into_rgba8(),
            Err(e) => {
                eprintln!("error: {} is not a valid image: {e}", path.display());
                return;
            }
        };

        let (img_w, img_h) = img.dimensions();
        let scale = (MAX_DROP_DIM / img_w as f32)
            .min(MAX_DROP_DIM / img_h as f32)
            .min(1.0);
        let w = img_w as f32 * scale;
        let h = img_h as f32 * scale;

        // Centre the sprite on the cursor.
        let x = (self.cursor_pos.0 - w / 2.0).max(0.0);
        let y = (self.cursor_pos.1 - h / 2.0).max(0.0);

        let image = ImageData::rgba(img_w, img_h, img.into_raw());
        let scene = self.scene.as_mut().unwrap();
        let z = scene.drawables.max_z() + 1.0;
        let mut sprite = Sprite::new(x, y, w, h, image);
        sprite.z = z;
        scene.drawables.push(sprite);
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("HaBoard")
                        .with_inner_size(winit::dpi::LogicalSize::new(800u32, 600u32)),
                )
                .expect("Failed to create window"),
        );

        let engine = pollster::block_on(Engine::new(window));
        let sprites = self.initial_sprites.take().unwrap_or_default();
        self.scene = Some(Scene::new(engine, sprites, self.mode));
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
                if let Some(scene) = &self.scene
                    && let Err(e) = save_scene(&scene.drawables)
                {
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
                // Track cursor position for drop placement.
                if let WindowEvent::CursorMoved { position, .. } = &event {
                    self.cursor_pos = (position.x as f32, position.y as f32);
                }
                // Track modifier keys for Ctrl+S.
                if let WindowEvent::ModifiersChanged(mods) = &event {
                    self.modifiers = mods.state();
                }
                // Ctrl+S: save immediately without forwarding to the scene.
                if let WindowEvent::KeyboardInput {
                    event: key_event, ..
                } = &event
                {
                    if key_event.state == ElementState::Pressed
                        && !key_event.repeat
                        && self.modifiers.control_key()
                        && key_event.physical_key == PhysicalKey::Code(KeyCode::KeyS)
                    {
                        if let Some(scene) = &self.scene {
                            if let Err(e) = save_scene(&scene.drawables) {
                                eprintln!("error: failed to save scene: {e}");
                            }
                        }
                        return;
                    }
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

    let sprites = load_scene().unwrap_or_else(|| {
        println!("No saved scene found, starting with defaults.");
        default_sprites()
    });

    let event_loop = EventLoop::new().expect("Failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(cli.mode.into(), sprites);
    event_loop.run_app(&mut app).expect("Event loop error");
}

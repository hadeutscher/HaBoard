use clap::{Parser, ValueEnum};
use haboard::{SceneMode, SceneRunner, Sprite, textures};

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
// Persistence
// ---------------------------------------------------------------------------

fn load_scene(path: &str) -> Option<Vec<Sprite>> {
    let bytes = std::fs::read(path).ok()?;
    match postcard::from_bytes::<Vec<Sprite>>(&bytes) {
        Ok(sprites) => Some(sprites),
        Err(e) => {
            eprintln!("warn: could not load {path}: {e}");
            None
        }
    }
}

fn save_scene(path: &str, sprites: &[Sprite]) {
    match postcard::to_allocvec(sprites) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(path, bytes) {
                eprintln!("error: failed to save scene: {e}");
            }
        }
        Err(e) => eprintln!("error: failed to serialize scene: {e}"),
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
// Entry point
// ---------------------------------------------------------------------------

const SAVE_PATH: &str = "scene.bin";

fn main() {
    let cli = Cli::parse();

    let sprites = load_scene(SAVE_PATH).unwrap_or_else(|| {
        println!("No saved scene found, starting with defaults.");
        default_sprites()
    });

    let mut runner = SceneRunner::new(sprites, cli.mode.into());
    runner.run();
    save_scene(SAVE_PATH, &runner.sprites());
}

use clap::{Parser, ValueEnum};
use haboard::{SceneMode, SceneRunner, Sprite, demo};

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
// Entry point
// ---------------------------------------------------------------------------

const SAVE_PATH: &str = "scene.bin";

fn main() {
    env_logger::init();

    let cli = Cli::parse();

    let sprites = load_scene(SAVE_PATH).unwrap_or_else(|| {
        println!("No saved scene found, starting with defaults.");
        demo::default_sprites()
    });

    let runner = SceneRunner::new(sprites, cli.mode.into());
    let final_sprites = runner.run();
    save_scene(SAVE_PATH, &final_sprites);
}

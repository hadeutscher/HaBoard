use clap::{Parser, ValueEnum};
use haboard::{FileStore, SceneMode, SceneRunner, SceneStore, demo};

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
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    env_logger::init();

    let cli = Cli::parse();

    // Persist the scene to the per-user app data directory; the runner autosaves
    // through this store after each edit.
    let store = FileStore::app_data();
    let sprites = store
        .as_ref()
        .and_then(|s| s.load())
        .unwrap_or_else(demo::default_sprites);

    let mut runner = SceneRunner::new(sprites, cli.mode.into());
    if let Some(store) = store {
        runner = runner.with_store(Box::new(store));
    }
    runner.run();
}

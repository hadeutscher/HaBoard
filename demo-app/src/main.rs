use clap::{Parser, ValueEnum};
use haboard::{DroppedImage, FileStore, SceneMode, SceneRunner, SceneStore, Sprite, demo};

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

    // Load from the per-user app data directory; persist back on each edit.
    let store = FileStore::app_data();
    let sprites = store
        .as_ref()
        .and_then(|s: &FileStore| SceneStore::<Sprite>::load(s))
        .unwrap_or_else(demo::default_sprites);

    let mut runner = SceneRunner::new(sprites, cli.mode.into());
    if let Some(store) = store {
        runner = runner.on_change(move |scene| {
            let items: Vec<Sprite> = scene.drawables.iter().cloned().collect();
            store.save(&items);
        });
    }
    runner =
        runner.on_drop_image(|d: DroppedImage| Sprite::new(d.x, d.y, d.width, d.height, d.image));
    runner.run();
}

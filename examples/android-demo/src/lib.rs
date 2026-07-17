//! Android example for haboard.
//!
//! Renders the shared demo scene. Build and run with cargo-apk:
//! `cargo apk run` (requires the Android SDK + NDK and an installed
//! `aarch64-linux-android` / `x86_64-linux-android` Rust target).

use haboard::{FileStore, SceneMode, SceneRunner, SceneStore, Sprite, UserEvent, demo};
use winit::{
    event_loop::EventLoop,
    platform::android::{EventLoopBuilderExtAndroid, activity::AndroidApp},
};

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    // Persist into the app's private data dir; capture it before `app` is moved
    // into the event loop. Save back on each committing edit via on_change.
    let store = app.internal_data_path().map(FileStore::in_dir);

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .with_android_app(app)
        .build()
        .expect("Failed to build Android event loop");

    let sprites = store
        .as_ref()
        .and_then(|s: &FileStore| SceneStore::<Sprite>::load(s))
        .unwrap_or_else(demo::default_sprites);
    // Edit mode so the demo sprites (which default to `locked`) can be selected
    // and dragged by touch; Run mode would block dragging locked sprites.
    let mut runner = SceneRunner::new(sprites, SceneMode::Edit);
    if let Some(store) = store {
        runner = runner.on_change(move |scene| {
            let items: Vec<Sprite> = scene.drawables.iter().cloned().collect();
            store.save(&items);
        });
    }
    runner.run_with(event_loop);
}

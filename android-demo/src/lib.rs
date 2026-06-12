//! Android example for haboard.
//!
//! Renders the shared demo scene. Build and run with cargo-apk:
//! `cargo apk run` (requires the Android SDK + NDK and an installed
//! `aarch64-linux-android` / `x86_64-linux-android` Rust target).

use haboard::{SceneMode, SceneRunner, UserEvent, demo};
use winit::event_loop::EventLoop;
use winit::platform::android::{EventLoopBuilderExtAndroid, activity::AndroidApp};

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .with_android_app(app)
        .build()
        .expect("Failed to build Android event loop");

    let sprites = demo::default_sprites();
    // Edit mode so the demo sprites (which default to `locked`) can be selected
    // and dragged by touch; Run mode would block dragging locked sprites.
    let runner = SceneRunner::new(sprites, SceneMode::Edit);
    runner.run_with(event_loop);
}

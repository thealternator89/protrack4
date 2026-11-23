use gpui_kit::component::TitleBar;
use gpui_kit::{AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use protrack4::ui::WorkspaceShell;
use tracing_subscriber::EnvFilter;

fn main() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("protrack4=info,warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    tracing::info!("Starting ProTrack (Phase 1 shell)");

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            protrack4::ui::register_keybindings(cx);

            let window_bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(window_bounds)),
                window_min_size: Some(size(px(860.), px(560.))),
                ..TitleBar::window_options()
            };

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| WorkspaceShell::new(window, cx))
            })
            .expect("failed to open ProTrack main window");

            cx.activate(true);
        });
}

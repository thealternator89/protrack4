use gpui_kit::component::TitleBar;
use gpui_kit::{AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use protrack4::state::LaunchConfig;
use protrack4::ui::WorkspaceShell;
use protrack4::workspace::parse_cli_db_arg;
use tracing_subscriber::EnvFilter;

fn main() {
    let db_override = match parse_cli_db_arg(std::env::args()) {
        Ok(path) => path,
        Err(msg) => {
            eprintln!("protrack4: {msg}");
            std::process::exit(2);
        }
    };

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("protrack4=info,warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    tracing::info!(db_override = ?db_override, "Starting ProTrack (Phase 2)");

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            protrack4::ui::register_keybindings(cx);

            let window_bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(window_bounds)),
                window_min_size: Some(size(px(860.), px(560.))),
                ..TitleBar::window_options()
            };

            let launch_config = LaunchConfig {
                db_override,
                preferences_path: None,
            };

            let (_, shell) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| WorkspaceShell::new_with_config(launch_config, window, cx))
            })
            .expect("failed to open ProTrack main window");

            let app_state = shell.read(cx).app_state().clone();
            cx.on_app_quit(move |cx| {
                let _ = app_state.update(cx, |app, cx| app.close_workspace_sync(cx));
                async {}
            })
            .detach();

            cx.activate(true);
        });
}

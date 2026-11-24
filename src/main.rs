#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::time::Duration;

use gpui_kit::component::TitleBar;
use gpui_kit::{AppContext as _, Bounds, WindowBounds, px, size};
use protrack4::branding::{self, AppAssets};
use protrack4::state::LaunchConfig;
use protrack4::ui::WorkspaceShell;
use protrack4::workspace::{install_signal_handlers, parse_cli_db_arg, take_shutdown_signal};
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
    tracing::info!(db_override = ?db_override, "Starting ProTrack4");
    install_signal_handlers();

    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            branding::set_app_dock_icon();
            protrack4::ui::register_keybindings(cx);

            let window_bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
            let mut options = TitleBar::window_options();
            options.window_bounds = Some(WindowBounds::Windowed(window_bounds));
            options.window_min_size = Some(size(px(860.), px(560.)));
            options.app_id = Some("nz.thealternator.protrack4.app".to_string());
            if let Some(ref mut titlebar) = options.titlebar {
                titlebar.title = Some("ProTrack4".into());
            }

            let launch_config = LaunchConfig {
                db_override,
                preferences_path: None,
            };

            let (_, shell) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| WorkspaceShell::new_with_config(launch_config, window, cx))
            })
            .expect("failed to open ProTrack4 main window");

            let app_state_quit = shell.read(cx).app_state().clone();
            let inspector_quit = shell.read(cx).inspector().clone();
            cx.on_app_quit(move |cx| {
                inspector_quit.update(cx, |view, cx| view.flush_pending_edits_for_shutdown(cx));
                let _ = app_state_quit.update(cx, |app, cx| app.close_workspace_sync(cx));
                async {}
            })
            .detach();

            let app_state_sig = shell.read(cx).app_state().clone();
            let inspector_sig = shell.read(cx).inspector().clone();
            cx.spawn(async move |cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    if take_shutdown_signal() {
                        cx.update(|cx| {
                            inspector_sig
                                .update(cx, |view, cx| view.flush_pending_edits_for_shutdown(cx));
                            if let Err(err) =
                                app_state_sig.update(cx, |app, cx| app.close_workspace_sync(cx))
                            {
                                tracing::warn!(
                                    error = %err,
                                    "Failed to flush workspace cleanly on shutdown signal"
                                );
                            }
                            cx.quit();
                        });
                        break;
                    }
                }
            })
            .detach();

            cx.activate(true);
        });
}

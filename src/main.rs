// Hide the console window for release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, anyhow};
use rustclick::app::AppService;
use rustclick::core::config::Config;
use rustclick::storage::ConfigStore;
use rustclick::ui::AppWindow;
use rustclick::{controller, logging};
use slint::ComponentHandle;

fn main() -> anyhow::Result<()> {
    let store = ConfigStore::default_location();
    let (config, mut warnings) = match &store {
        Some(store) => {
            let loaded = store.load();
            (loaded.config, loaded.warnings)
        }
        None => (
            Config::default(),
            vec![
                "No per-user configuration directory is available; settings will not be saved."
                    .into(),
            ],
        ),
    };

    let log = logging::init(config.logging.level, ConfigStore::log_file().as_deref());
    let log_file = match log.file() {
        Ok(path) => path.map(|p| p.display().to_string()),
        Err(e) => {
            warnings.push(e.to_owned());
            None
        }
    };
    tracing::info!(
        version = %rustclick::version(),
        os = %rustclick::platform::environment(),
        config = ?store.as_ref().map(|s| s.path().to_owned()),
        mode = config.input_mode.label(),
        "starting RustClick"
    );

    let mut service = AppService::new(config, store, warnings);
    service.set_log_file(log_file);
    service.set_log_level_hook(Box::new(move |level| log.set_level(level)));
    let sender = service.sender();

    let ui = AppWindow::new().context("could not create the main window")?;
    controller::bind(&ui, sender.clone());

    // All core state lives on the service thread. It publishes view models,
    // which are applied on the UI thread through Slint's event loop.
    let weak = ui.as_weak();
    let worker = std::thread::Builder::new()
        .name("rustclick-service".into())
        .spawn(move || {
            service.run(move |view| {
                // Fails only once the event loop has stopped; nothing to update then.
                let _ = weak.upgrade_in_event_loop(move |ui| controller::apply(&ui, view));
            });
        })
        .context("could not start the service thread")?;

    ui.run().context("UI event loop failed")?;

    // Stop providers and save configuration before exiting.
    sender.shutdown();
    worker
        .join()
        .map_err(|_| anyhow!("the service thread panicked"))?;
    Ok(())
}

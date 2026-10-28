//! Configuration persistence through `ConfigStore` and the service.

mod common;

use common::*;
use rustclick::app::{AppService, Command, Msg};
use rustclick::core::config::{CONFIG_VERSION, Config};
use rustclick::core::input::{ButtonId, DeviceId};
use rustclick::core::mappings::Action;
use rustclick::core::targets::SelectionMode;
use rustclick::input::mock::MockProfile;
use rustclick::storage::ConfigStore;
use rustclick::targets::mock::{MockApp, MockTargetProvider};

fn start(store: &ConfigStore, apps: &[MockApp]) -> AppService {
    let loaded = store.load();
    let mut world = MockTargetProvider::new();
    for app in apps {
        world.launch(*app);
    }
    AppService::with_mock_targets(loaded.config, Some(store.clone()), loaded.warnings, world)
}

fn stop(mut service: AppService) {
    service.handle(Msg::Shutdown);
}

#[test]
fn missing_file_yields_defaults_without_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let loaded = ConfigStore::in_dir(dir.path()).load();
    assert_eq!(loaded.config, Config::default());
    assert!(loaded.warnings.is_empty());
}

#[test]
fn save_and_load_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::in_dir(dir.path().join("nested"));
    let mut config = Config::default();
    config.preferences.auto_select_single = false;
    store.save(&config).unwrap();
    let loaded = store.load();
    assert_eq!(loaded.config, config);
    assert!(loaded.warnings.is_empty());
}

#[test]
fn invalid_file_is_kept_aside_and_defaults_are_used() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::in_dir(dir.path());
    std::fs::write(store.path(), "version = 1\ninput_mode = [").unwrap();
    let loaded = store.load();
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.warnings.len(), 1);
    assert!(!store.path().exists());
    let backups: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".invalid-"))
        .collect();
    assert_eq!(backups.len(), 1);
}

#[test]
fn newer_config_versions_are_not_misread() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::in_dir(dir.path());
    std::fs::write(store.path(), format!("version = {}\n", CONFIG_VERSION + 1)).unwrap();
    let loaded = store.load();
    assert_eq!(loaded.config, Config::default());
    assert!(
        loaded.warnings[0].contains("not supported"),
        "{:?}",
        loaded.warnings
    );
}

#[test]
fn invalid_settings_are_refused_on_save() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::in_dir(dir.path());
    let mut config = Config::default();
    config.preferences.change_target_shortcut = "T".into();
    assert!(store.save(&config).is_err());
    assert!(!store.path().exists());
}

#[test]
fn user_settings_persist_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::in_dir(dir.path());

    let mut first = start(&store, &[MockApp::PowerPoint]);
    first.execute(Command::AddMockDevice(MockProfile::TwoButton));
    first.execute(Command::RenameDevice(
        DeviceId::new("mock-2"),
        "Stage left".into(),
    ));
    first.execute(Command::SetDeviceEnabled(remote(), false));
    first.execute(Command::SetMapping {
        button: ButtonId::standard("aux"),
        action: Some(Action::custom("notes").unwrap()),
    });
    first.execute(Command::SetSelectionMode(SelectionMode::Process));
    first.execute(Command::SetShortcut("Ctrl+Shift+F9".into()));
    let first_pid = processes_of(&first, "PowerPoint")[0].pid;
    stop(first);

    // A different mock world: PowerPoint gets another PID this time.
    let second = start(&store, &[MockApp::TextEditor, MockApp::PowerPoint]);
    let config = second.config();
    assert_eq!(config.preferences.selection_mode, SelectionMode::Process);
    assert_eq!(config.preferences.change_target_shortcut, "Ctrl+Shift+F9");
    assert_eq!(
        config.mappings.resolve(&ButtonId::standard("aux")),
        Some(&Action::Custom("notes".into()))
    );

    let devices = second.devices();
    assert_eq!(devices.len(), 2);
    assert!(!devices.get(&remote()).unwrap().enabled);
    assert_eq!(
        devices.get(&DeviceId::new("mock-2")).unwrap().info.name,
        "Stage left"
    );

    // The saved target is matched by its stable identity, not its old PID.
    let new_process = processes_of(&second, "PowerPoint")[0];
    assert_ne!(new_process.pid, first_pid);
    assert_eq!(
        second.selection().binding().map(|b| b.process),
        Some(new_process)
    );
}

#[test]
fn persisted_file_contains_no_transient_identifiers() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::in_dir(dir.path());
    let s = start(&store, &[MockApp::PowerPoint]);
    stop(s);
    let text = std::fs::read_to_string(store.path()).unwrap();
    assert!(text.contains("[saved_target"), "{text}");
    assert!(text.contains("POWERPNT.EXE"), "{text}");
    assert!(!text.contains("pid"), "{text}");
    assert!(!text.contains("instance"), "{text}");
    assert!(!text.contains("window_id"), "{text}");
}

#[test]
fn reset_restores_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::in_dir(dir.path());
    let mut s = start(&store, &[MockApp::PowerPoint]);
    s.execute(Command::AddMockDevice(MockProfile::TwoButton));
    s.execute(Command::SetAutoSelectSingle(false));
    s.execute(Command::ResetConfig);

    assert_eq!(s.devices().len(), 1);
    // The only eligible target is selected again under the default policy.
    assert!(s.config().saved_target.is_some());
    let expected = Config {
        saved_target: s.config().saved_target.clone(),
        ..Config::default()
    };
    assert_eq!(s.config(), &expected);
    assert_eq!(store.load().config, expected);
}

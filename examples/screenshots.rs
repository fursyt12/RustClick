//! Renders every page of the UI to PNG files with Slint's software renderer,
//! without opening a window or touching the real configuration. Useful for
//! reviewing UI changes and for documentation.
//!
//! Usage: `cargo run --example screenshots -- [output-dir] [width] [height]`

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{Context, anyhow};
use rustclick::app::{AppService, Command};
use rustclick::controller;
use rustclick::core::config::Config;
use rustclick::core::input::{DeviceId, EventKind};
use rustclick::input::mock::MockProfile;
use rustclick::targets::mock::{MockApp, MockTargetProvider};
use rustclick::ui::{AppState, AppWindow};
use slint::language::ColorScheme;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, PlatformError, Rgb8Pixel};

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn capture(window: &MinimalSoftwareWindow, size: PhysicalSize, path: &Path) -> anyhow::Result<()> {
    slint::platform::update_timers_and_animations();
    window.request_redraw();
    let (w, h) = (size.width as usize, size.height as usize);
    let mut pixels = vec![Rgb8Pixel::default(); w * h];
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, w);
    });
    let mut encoder =
        png::Encoder::new(BufWriter::new(File::create(path)?), size.width, size.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let data: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    encoder.write_header()?.write_image_data(&data)?;
    println!("wrote {}", path.display());
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().unwrap_or_else(|| "screenshots".into()));
    let width = args.next().map_or(Ok(560), |s| s.parse())?;
    let height = args.next().map_or(Ok(680), |s| s.parse())?;
    std::fs::create_dir_all(&out).context("creating output directory")?;

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone())))
        .map_err(|e| anyhow!("{e:?}"))?;
    let size = PhysicalSize::new(width, height);
    window.set_size(size);

    // A small scenario: two remotes, PowerPoint routed, a few events.
    let mut world = MockTargetProvider::new();
    world.launch(MockApp::PowerPoint);
    world.launch(MockApp::TextEditor);
    let mut service = AppService::with_mock_targets(Config::default(), None, Vec::new(), world);
    service.execute(Command::AddMockDevice(MockProfile::TwoButton));
    service.execute(Command::StartRouting);
    for (device, button, kind) in [
        ("mock-1", "forward", EventKind::Click),
        ("mock-1", "forward", EventKind::Click),
        ("mock-2", "back", EventKind::Press),
        ("mock-2", "back", EventKind::Release),
        ("mock-1", "aux", EventKind::Click),
    ] {
        service.execute(Command::EmitMockEvent {
            device: DeviceId::new(device),
            button: button.into(),
            kind,
        });
    }
    service.execute(Command::ReplayLastEvent(DeviceId::new("mock-1")));

    let ui = AppWindow::new()?;
    controller::bind(&ui, service.sender());
    controller::apply(&ui, service.view());
    ui.show()?;
    let state = ui.global::<AppState>();

    for (scheme, suffix) in [(ColorScheme::Light, ""), (ColorScheme::Dark, "-dark")] {
        ui.set_color_scheme(scheme);
        for (page, name) in [
            (0, "home"),
            (1, "log"),
            (2, "target"),
            (3, "settings"),
            (4, "simulator"),
        ] {
            state.set_page(page);
            capture(&window, size, &out.join(format!("{name}{suffix}.png")))?;
        }
    }
    ui.set_color_scheme(ColorScheme::Light);

    // Ambiguity: a second eligible application and an explicit change request.
    service.execute(Command::MockLaunch(MockApp::Acrobat));
    service.execute(Command::RequestTargetChange);
    controller::apply(&ui, service.view());
    state.set_page(0);
    capture(&window, size, &out.join("target-prompt.png"))?;

    ui.hide()?;
    Ok(())
}

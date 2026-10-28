//! `rustclick-diag`: records how presentation remotes and target applications
//! behave on this Windows machine, so RustClick's native adapter can be
//! debugged without access to the hardware. Everything it prints is also
//! written to a text file next to the executable; that file is what gets
//! sent back. See `docs/windows-testing.md`.

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("rustclick-diag records Windows input and window details and only runs on Windows.");
    std::process::exit(2);
}

#[cfg(target_os = "windows")]
fn main() {
    if let Err(e) = diag::run() {
        eprintln!("\nrustclick-diag failed: {e:#}");
        diag::pause();
        std::process::exit(1);
    }
}

#[cfg(target_os = "windows")]
mod diag {
    use std::collections::{BTreeMap, HashMap};
    use std::fs::File;
    use std::io::{BufRead, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use anyhow::Context;
    use hidapi::HidApi;
    use rustclick::platform::windows::desktop::{self, TopWindow};
    use rustclick::platform::windows::hidpp::{self, Control, Hidpp, HidppError};
    use rustclick::platform::windows::keys;
    use rustclick::platform::windows::raw_input::{
        self, Capture, CaptureEvent, CaptureOptions, RawDevice,
    };

    const CAPTURE_LIMIT: Duration = Duration::from_secs(180);

    /// Lines go through a writer thread so the capture thread (and with it
    /// the low-level hook) never waits for console or disk output.
    #[derive(Clone)]
    struct Report {
        tx: Sender<String>,
        start: Instant,
    }

    impl Report {
        fn create() -> anyhow::Result<(Self, PathBuf, JoinHandle<()>)> {
            let name = format!(
                "rustclick-diag-{}.txt",
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            );
            let beside_exe = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join(&name)));
            let (file, path) = match beside_exe.map(|p| (File::create(&p), p)) {
                Some((Ok(f), p)) => (f, p),
                _ => {
                    let p = std::env::temp_dir().join(&name);
                    (File::create(&p).context("creating the report file")?, p)
                }
            };
            let (tx, rx) = mpsc::channel::<String>();
            let writer = std::thread::spawn(move || {
                let mut file = std::io::BufWriter::new(file);
                for line in rx {
                    println!("{line}");
                    let _ = writeln!(file, "{line}");
                    let _ = file.flush();
                }
            });
            Ok((
                Self {
                    tx,
                    start: Instant::now(),
                },
                path,
                writer,
            ))
        }

        fn raw(&self, line: impl Into<String>) {
            let _ = self.tx.send(line.into());
        }

        fn at(&self, at: Instant, line: impl AsRef<str>) {
            let t = at.saturating_duration_since(self.start).as_secs_f64();
            self.raw(format!("[{t:>10.4}] {}", line.as_ref()));
        }

        fn line(&self, line: impl AsRef<str>) {
            self.at(Instant::now(), line);
        }

        fn section(&self, title: &str) {
            self.raw("");
            self.raw(format!("==== {title} ===="));
        }
    }

    /// Console lines, read on a separate thread so waits can time out.
    fn stdin_lines() -> &'static Mutex<Receiver<String>> {
        static LINES: OnceLock<Mutex<Receiver<String>>> = OnceLock::new();
        LINES.get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                for line in std::io::stdin().lock().lines() {
                    if tx.send(line.unwrap_or_default()).is_err() {
                        break;
                    }
                }
            });
            Mutex::new(rx)
        })
    }

    /// Waits for Enter (returns the typed line) or the timeout.
    fn wait_line(timeout: Duration) -> Option<String> {
        let rx = stdin_lines().lock().unwrap_or_else(|e| e.into_inner());
        // Drop anything typed earlier (remotes may "type" B or . into the console).
        while rx.try_recv().is_ok() {}
        rx.recv_timeout(timeout).ok()
    }

    /// Asks a 1/0 question; digits work with any keyboard layout.
    fn ask_yes(report: &Report, question: &str) -> bool {
        report.raw(format!(">>> {question}  [1 = yes, 0 = no, then Enter]"));
        let answer = wait_line(Duration::from_secs(600))
            .and_then(|l| l.chars().rev().find(char::is_ascii_digit));
        let yes = answer == Some('1');
        report.raw(format!("    answer: {}", if yes { "yes" } else { "no" }));
        yes
    }

    pub fn pause() {
        println!("\nPress Enter to close this window.");
        let _ = wait_line(Duration::from_secs(3600));
    }

    pub fn run() -> anyhow::Result<()> {
        let (report, path, writer) = Report::create()?;
        report.raw(format!("RustClick diagnostics {}", rustclick::version()));
        report.raw(format!(
            "Started {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z")
        ));
        report.raw(format!("Report file: {}", path.display()));
        system_info(&report);
        logitech_software(&report);

        let api = HidApi::new().context("initializing the HID API")?;
        hid_devices(&report, &api);
        let raw_devices = raw_devices(&report);

        report.section("HID++ probe");
        if api
            .device_list()
            .any(|d| hidpp::is_hidpp_collection(d.vendor_id(), d.usage_page(), d.usage()))
        {
            report.raw(
                ">>> Press any button on EACH remote now so it wakes up, then press Enter here.",
            );
            let _ = wait_line(Duration::from_secs(300));
        }
        let (mut links, probed) = probe_hidpp(&report, &api);

        report.section("Capture 1: normal keystrokes");
        report.raw(">>> Press every button of every remote, one at a time:");
        report.raw("    1) short press   2) long press (hold 2 s)   3) double press");
        report.raw("    Then press Enter here (the capture also ends after 3 minutes).");
        report.raw(
            "    All keystrokes are recorded while the capture runs: do not type anything private.",
        );
        links = capture(&report, &raw_devices, links, &probed);

        let divertable = probed
            .iter()
            .any(|d| d.controls.iter().any(Control::divertable));
        if divertable
            && ask_yes(
                &report,
                "Run the button-diversion test? Buttons will temporarily report to this tool instead of typing keys.",
            )
        {
            links = divert_test(&report, &raw_devices, links, &probed);
        } else if !divertable {
            report.raw("No divertable HID++ controls were found; skipping the diversion test.");
        }
        drop(links);

        windows(&report);
        if ask_yes(
            &report,
            "Run the delivery test? Open PowerPoint (ideally a slide show) or a PDF first.",
        ) {
            delivery_test(&report);
        }

        report.section("Done");
        report.raw(format!("Please send this file: {}", path.display()));
        drop(report);
        let _ = writer.join();
        pause();
        Ok(())
    }

    fn system_info(report: &Report) {
        report.section("System");
        let ver = std::process::Command::new("cmd")
            .args(["/c", "ver"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_else(|e| format!("unknown ({e})"));
        report.raw(format!("OS: {ver}"));
        report.raw(format!(
            "Architecture: {} · elevated: {}",
            std::env::consts::ARCH,
            // SAFETY: plain query.
            unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
        ));
    }

    fn logitech_software(report: &Report) {
        report.section("Logitech software");
        let found: Vec<_> = desktop::processes()
            .into_iter()
            .filter(|(_, name)| {
                let n = name.to_ascii_lowercase();
                n.contains("logi") || n.contains("lghub") || n.contains("presentation")
            })
            .collect();
        if found.is_empty() {
            report.raw("No Logitech processes running.");
        }
        for (pid, name) in found {
            report.raw(format!("PID {pid}: {name}"));
        }
        report.raw("(Logitech Presentation / Options+ can intercept remote buttons themselves.)");
    }

    fn hid_devices(report: &Report, api: &HidApi) {
        report.section("HID collections");
        let mut others = 0;
        for d in api.device_list() {
            if d.vendor_id() != hidpp::LOGITECH_VID {
                others += 1;
                continue;
            }
            report.raw(format!(
                "046D:{:04X} page 0x{:04X} usage 0x{:04X} if {} · \"{}\" by \"{}\" serial \"{}\" rel 0x{:04X}{}",
                d.product_id(),
                d.usage_page(),
                d.usage(),
                d.interface_number(),
                d.product_string().unwrap_or(""),
                d.manufacturer_string().unwrap_or(""),
                d.serial_number().unwrap_or(""),
                d.release_number(),
                if hidpp::is_hidpp_collection(d.vendor_id(), d.usage_page(), d.usage()) {
                    " · HID++"
                } else {
                    ""
                }
            ));
            report.raw(format!("    {}", d.path().to_string_lossy()));
        }
        report.raw(format!(
            "{others} collection(s) of other vendors not listed."
        ));
    }

    fn raw_devices(report: &Report) -> Vec<RawDevice> {
        report.section("Raw Input devices");
        let devices = raw_input::list_devices();
        for (i, d) in devices.iter().enumerate() {
            let ids = match (d.vid, d.pid) {
                (Some(v), Some(p)) => format!("{v:04X}:{p:04X}"),
                _ => "----:----".into(),
            };
            let mark = if d.vid == Some(hidpp::LOGITECH_VID) {
                "*"
            } else {
                " "
            };
            report.raw(format!(
                "{mark}#{i:<2} {:<8} {ids} page 0x{:04X} usage 0x{:04X} handle 0x{:x} {}",
                d.kind.label(),
                d.usage_page,
                d.usage,
                d.handle,
                d.info
            ));
            report.raw(format!("     {}", d.name));
        }
        report.raw("(* = Logitech)");
        devices
    }

    /// A HID++ 2.0 device found during the probe.
    struct Probed {
        path: String,
        index: u8,
        name: String,
        reprog: Option<u8>,
        controls: Vec<Control>,
    }

    fn probe_hidpp(report: &Report, api: &HidApi) -> (Vec<Hidpp>, Vec<Probed>) {
        let mut links = Vec::new();
        let mut probed = Vec::new();
        let mut seen = Vec::new();
        for d in api.device_list() {
            if !hidpp::is_hidpp_collection(d.vendor_id(), d.usage_page(), d.usage()) {
                continue;
            }
            let path = d.path().to_string_lossy().into_owned();
            if seen.contains(&path) {
                continue;
            }
            seen.push(path.clone());
            report.raw(format!(
                "Collection 046D:{:04X} \"{}\" {path}",
                d.product_id(),
                d.product_string().unwrap_or("")
            ));
            let mut link = match Hidpp::open(api, d.path()) {
                Ok(link) => link,
                Err(e) => {
                    report.raw(format!("  cannot open: {e}"));
                    continue;
                }
            };
            for index in std::iter::once(hidpp::DIRECT).chain(1..=6) {
                match link.protocol_version(index) {
                    Ok((major, minor)) => {
                        report.raw(format!("  index 0x{index:02X}: HID++ {major}.{minor}"));
                        if major >= 2
                            && let Some(p) = describe_device(report, &mut link, &path, index)
                        {
                            probed.push(p);
                        }
                    }
                    Err(HidppError::Timeout) => {
                        report.raw(format!("  index 0x{index:02X}: no response"))
                    }
                    Err(e) => report.raw(format!("  index 0x{index:02X}: {e}")),
                }
            }
            for r in link.stray.drain(..) {
                report.raw(format!("  unsolicited report: {}", hidpp::hex(&r)));
            }
            links.push(link);
        }
        if seen.is_empty() {
            report.raw("No HID++ collections found (is a Logitech receiver plugged in or the remote paired?).");
        }
        (links, probed)
    }

    fn describe_device(report: &Report, link: &mut Hidpp, path: &str, index: u8) -> Option<Probed> {
        let features = match link.features(index) {
            Ok(f) => f,
            Err(e) => {
                report.raw(format!("    features: {e}"));
                return None;
            }
        };
        let find = |id| features.iter().find(|f| f.1 == id).map(|f| f.0);
        for (i, id, kind, version) in &features {
            report.raw(format!(
                "    [{i:>2}] 0x{id:04X} {:<24} type 0x{kind:02x} v{version}",
                hidpp::feature_name(*id)
            ));
        }
        let name = match find(hidpp::FEATURE_DEVICE_NAME).map(|f| link.device_name(index, f)) {
            Some(Ok(name)) => name,
            Some(Err(e)) => format!("(name error: {e})"),
            None => "(no name feature)".into(),
        };
        report.raw(format!("    name: {name}"));
        if let Some(f) = find(hidpp::FEATURE_BATTERY_STATUS) {
            match link.request(index, f, 0, &[], Duration::from_millis(500)) {
                Ok(r) => report.raw(format!(
                    "    battery: {}% (next {}%, status {})",
                    r[0], r[1], r[2]
                )),
                Err(e) => report.raw(format!("    battery: {e}")),
            }
        }
        if let Some(f) = find(hidpp::FEATURE_UNIFIED_BATTERY) {
            match link.request(index, f, 1, &[], Duration::from_millis(500)) {
                Ok(r) => report.raw(format!(
                    "    unified battery: {}% level 0x{:02x} charging {} external {}",
                    r[0], r[1], r[2], r[3]
                )),
                Err(e) => report.raw(format!("    unified battery: {e}")),
            }
        }
        let reprog = find(hidpp::FEATURE_REPROG_CONTROLS_V4);
        let mut controls = Vec::new();
        if let Some(f) = reprog {
            match link.controls(index, f) {
                Ok(list) => {
                    for c in &list {
                        let reporting = link
                            .control_reporting(index, f, c.cid)
                            .map_or_else(|e| e.to_string(), |r| hidpp::hex(&r));
                        report.raw(format!(
                            "    control CID 0x{:04X} TID 0x{:04X} flags 0x{:02x} ({}) pos {} group {} gmask 0x{:02x} flags2 0x{:02x} · reporting {reporting}",
                            c.cid,
                            c.tid,
                            c.flags,
                            c.describe_flags(),
                            c.position,
                            c.group,
                            c.group_mask,
                            c.flags2
                        ));
                    }
                    controls = list;
                }
                Err(e) => report.raw(format!("    controls: {e}")),
            }
        }
        Some(Probed {
            path: path.to_owned(),
            index,
            name,
            reprog,
            controls,
        })
    }

    /// Short labels for Raw Input handles, resolved lazily for new devices.
    fn device_label(devices: &[RawDevice], handle: usize) -> String {
        let ids = |d: &RawDevice| match (d.vid, d.pid) {
            (Some(v), Some(p)) => format!("{v:04X}:{p:04X}"),
            _ => "----:----".into(),
        };
        match devices.iter().position(|d| d.handle == handle) {
            Some(i) => format!("#{i} [{}]", ids(&devices[i])),
            None => match raw_input::device(handle) {
                Some(d) => format!("new 0x{handle:x} [{}] {}", ids(&d), d.name),
                None => format!("unknown 0x{handle:x}"),
            },
        }
    }

    fn window_label(hwnd: usize) -> String {
        format!(
            "0x{hwnd:x} {} \"{}\"",
            desktop::window_class(hwnd),
            desktop::window_text(hwnd)
                .chars()
                .take(40)
                .collect::<String>()
        )
    }

    /// Records Raw Input, the low-level hook and HID++ reports until Enter
    /// or the time limit. Returns the HID++ links for further use.
    fn capture(
        report: &Report,
        devices: &[RawDevice],
        links: Vec<Hidpp>,
        probed: &[Probed],
    ) -> Vec<Hidpp> {
        let devices: Arc<Vec<RawDevice>> = Arc::new(devices.to_vec());
        let moves: Arc<Mutex<BTreeMap<String, u64>>> = Arc::default();
        let handler = {
            let report = report.clone();
            let devices = devices.clone();
            let moves = moves.clone();
            Box::new(move |e: &CaptureEvent| {
                match *e {
                    CaptureEvent::Key {
                        device,
                        vkey,
                        make_code,
                        flags,
                        message,
                        msg_time,
                        at,
                    } => report.at(
                        at,
                        format!(
                            "RAW   {:<4} {:<9} vk=0x{vkey:02x} make=0x{make_code:02x} flags=0x{flags:x} msg=0x{message:x} time={msg_time} dev={}",
                            if CaptureEvent::is_key_up(flags) { "up" } else { "down" },
                            keys::vk_name(vkey),
                            device_label(&devices, device)
                        ),
                    ),
                    CaptureEvent::Hook {
                        vk,
                        scan,
                        flags,
                        time,
                        extra,
                        at,
                    } => report.at(
                        at,
                        format!(
                            "HOOK  {:<4} {:<9} vk=0x{vk:02x} scan=0x{scan:02x} flags=0x{flags:x}{} time={time} extra=0x{extra:x} fg={}",
                            if flags & 0x80 != 0 { "up" } else { "down" },
                            keys::vk_name(vk as u16),
                            if flags & 0x10 != 0 { " INJECTED" } else { "" },
                            window_label(desktop::foreground_window())
                        ),
                    ),
                    CaptureEvent::Hid {
                        device,
                        report: ref data,
                        at,
                    } => report.at(
                        at,
                        format!(
                            "HID   {} dev={}",
                            hidpp::hex(data),
                            device_label(&devices, device)
                        ),
                    ),
                    CaptureEvent::MouseButtons {
                        device,
                        button_flags,
                        button_data,
                        at,
                    } => report.at(
                        at,
                        format!(
                            "MOUSE buttons=0x{button_flags:04x} data=0x{button_data:04x} dev={}",
                            device_label(&devices, device)
                        ),
                    ),
                    CaptureEvent::MouseMove { device, .. } => {
                        let mut m = moves.lock().unwrap_or_else(|e| e.into_inner());
                        *m.entry(device_label(&devices, device)).or_default() += 1;
                    }
                    CaptureEvent::DeviceArrived(h) => {
                        if !devices.iter().any(|d| d.handle == h) {
                            report.line(format!("DEVICE arrived {}", device_label(&devices, h)));
                        }
                    }
                    CaptureEvent::DeviceRemoved(h) => {
                        report.line(format!("DEVICE removed 0x{h:x}"))
                    }
                }
                false
            })
        };
        let options = CaptureOptions {
            keyboard: true,
            consumer: true,
            mouse: true,
            keyboard_hook: true,
        };
        let mut capture = match Capture::start(options, handler) {
            Ok(c) => Some(c),
            Err(e) => {
                report.raw(format!("Raw Input capture could not start: {e}"));
                None
            }
        };

        // One listener per HID++ collection.
        let stop = Arc::new(AtomicBool::new(false));
        let listeners: Vec<JoinHandle<Hidpp>> = links
            .into_iter()
            .map(|link| {
                let report = report.clone();
                let stop = stop.clone();
                let decode: HashMap<u8, u8> = probed
                    .iter()
                    .filter(|p| p.path == link.path)
                    .filter_map(|p| Some((p.index, p.reprog?)))
                    .collect();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match link.read(Duration::from_millis(200)) {
                            Ok(Some(r)) => {
                                let at = Instant::now();
                                let pressed = r
                                    .get(1)
                                    .and_then(|i| decode.get(i))
                                    .and_then(|&f| hidpp::diverted_buttons(&r, f));
                                let note = match pressed {
                                    Some(cids) if cids.is_empty() => {
                                        " → diverted: all released".into()
                                    }
                                    Some(cids) => format!(
                                        " → diverted pressed: {}",
                                        cids.iter()
                                            .map(|c| format!("CID 0x{c:04X}"))
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    ),
                                    None => String::new(),
                                };
                                report.at(at, format!("HID++ {}{note}", hidpp::hex(&r)));
                            }
                            Ok(None) => {}
                            Err(e) => {
                                report.line(format!("HID++ read error: {e}"));
                                break;
                            }
                        }
                    }
                    link
                })
            })
            .collect();

        report.line("capture running");
        if wait_line(CAPTURE_LIMIT).is_none() {
            report.line("capture time limit reached");
        }
        stop.store(true, Ordering::Relaxed);
        if let Some(c) = capture.as_mut() {
            c.stop();
        }
        let links = listeners
            .into_iter()
            .filter_map(|l| l.join().ok())
            .collect();
        report.line("capture stopped");
        for (device, count) in moves.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            report.raw(format!("mouse movement reports from {device}: {count}"));
        }
        links
    }

    fn divert_test(
        report: &Report,
        devices: &[RawDevice],
        mut links: Vec<Hidpp>,
        probed: &[Probed],
    ) -> Vec<Hidpp> {
        report.section("Capture 2: diverted buttons");
        let mut diverted = Vec::new();
        for p in probed {
            let (Some(feature), Some(link)) =
                (p.reprog, links.iter_mut().find(|l| l.path == p.path))
            else {
                continue;
            };
            for c in p.controls.iter().filter(|c| c.divertable()) {
                match link.set_divert(p.index, feature, c.cid, true) {
                    Ok(r) => {
                        report.raw(format!(
                            "{} (index 0x{:02X}): diverted CID 0x{:04X} → {}",
                            p.name,
                            p.index,
                            c.cid,
                            hidpp::hex(&r)
                        ));
                        diverted.push((p.path.clone(), p.index, feature, c.cid));
                    }
                    Err(e) => report.raw(format!(
                        "{}: divert CID 0x{:04X} failed: {e}",
                        p.name, c.cid
                    )),
                }
            }
        }
        report.raw(">>> Press every button again (short, long, double), then press Enter here.");
        report
            .raw("    Diverted buttons should now appear as HID++ lines and NOT as RAW/HOOK keys.");
        let mut links = capture(report, devices, links, probed);
        for (path, index, feature, cid) in diverted {
            let Some(link) = links.iter_mut().find(|l| l.path == path) else {
                continue;
            };
            match link.set_divert(index, feature, cid, false) {
                Ok(_) => report.raw(format!("restored CID 0x{cid:04X}")),
                Err(e) => report.raw(format!(
                    "restoring CID 0x{cid:04X} failed: {e} (switch the remote off and on to reset it)"
                )),
            }
        }
        links
    }

    fn windows(report: &Report) {
        report.section("Windows");
        let all = desktop::top_level_windows();
        let mut details = HashMap::new();
        let mut exe = |pid: u32| -> String {
            details
                .entry(pid)
                .or_insert_with(|| {
                    let d = desktop::process_details(pid);
                    d.executable.or(d.error).unwrap_or_else(|| "?".into())
                })
                .clone()
        };
        for w in all.iter().filter(|w| w.is_app_window()) {
            report.raw(format!(
                "0x{:x} pid {} \"{}\" class {} {}{}",
                w.hwnd,
                w.pid,
                w.title,
                w.class,
                exe(w.pid),
                if w.minimized { " (minimized)" } else { "" }
            ));
        }
        let presentation: Vec<u32> = {
            let mut pids: Vec<u32> = all
                .iter()
                .map(|w| w.pid)
                .filter(|&pid| {
                    let e = exe(pid).to_ascii_lowercase();
                    e.ends_with("powerpnt.exe")
                        || e.ends_with("acrobat.exe")
                        || e.ends_with("acrord32.exe")
                })
                .collect();
            pids.sort_unstable();
            pids.dedup();
            pids
        };
        for pid in presentation {
            report.raw(format!(
                "All top-level windows of PID {pid} ({}):",
                exe(pid)
            ));
            for w in all.iter().filter(|w| w.pid == pid) {
                report.raw(format!(
                    "    0x{:x} class {} \"{}\" visible {} cloaked {} owned {} tool {} · thread focus {}",
                    w.hwnd,
                    w.class,
                    w.title,
                    w.visible,
                    w.cloaked,
                    w.owned,
                    w.tool_window,
                    desktop::thread_focus(w.thread_id).map_or("none".into(), window_label)
                ));
            }
        }
    }

    fn delivery_test(report: &Report) {
        report.section("Delivery test");
        let candidates: Vec<TopWindow> = desktop::top_level_windows()
            .into_iter()
            .filter(|w| {
                w.is_app_window() || (w.visible && w.class == desktop::POWERPOINT_SLIDESHOW_CLASS)
            })
            .collect();
        for (i, w) in candidates.iter().enumerate() {
            report.raw(format!("  {i:>2}: {} — {} [{}]", w.title, w.class, w.pid));
        }
        report.raw(">>> Type the number of the PowerPoint or PDF window to test, then Enter (empty = skip).");
        let Some(choice) = wait_line(Duration::from_secs(600))
            .and_then(|l| l.trim().parse::<usize>().ok())
            .and_then(|i| candidates.get(i).cloned())
        else {
            report.raw("    skipped");
            return;
        };
        report.raw(format!("    chosen: {}", window_label(choice.hwnd)));

        // Every plausible recipient: the window, its slide show, and the
        // focused child of each.
        let mut recipients = vec![(choice.hwnd, "chosen window".to_owned())];
        if let Some(show) = desktop::powerpoint_slideshow(choice.pid)
            && show.hwnd != choice.hwnd
        {
            recipients.push((show.hwnd, "slide show window".into()));
        }
        for (hwnd, what) in recipients.clone() {
            if let Some((_, thread)) = desktop::window_owner(hwnd)
                && let Some(focus) = desktop::thread_focus(thread)
                && focus != hwnd
                && desktop::is_self_or_child(hwnd, focus)
            {
                recipients.push((focus, format!("focused child of the {what}")));
            }
        }
        report.raw("Keep THIS console in the foreground; the target must not have focus.");
        for (hwnd, what) in &recipients {
            for synchronous in [false, true] {
                let method = if synchronous {
                    "SendMessageTimeout"
                } else {
                    "PostMessage"
                };
                report.line(format!(
                    "{method} PageDown to {what}: {}",
                    window_label(*hwnd)
                ));
                let result = if synchronous {
                    desktop::send_key(*hwnd, keys::VK_NEXT, 1000)
                        .map(|(down, up)| format!("ok, results {down} / {up}"))
                } else {
                    desktop::post_key(*hwnd, keys::VK_NEXT).map(|()| "ok".to_owned())
                };
                report.line(format!("    result: {}", result.unwrap_or_else(|e| e)));
                ask_yes(report, "Did the slide or page move forward just now?");
            }
        }
    }
}

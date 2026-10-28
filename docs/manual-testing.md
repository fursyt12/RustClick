# Manual test procedures

Automated tests cover the routing engine, the target state machine, the
service and the UI bindings. These manual procedures cover what automation
does not: the real window, rendering, resizing, and platform specifics.

Use an isolated configuration for every session:

```bash
RUSTCLICK_CONFIG_DIR=$(mktemp -d) cargo run
```

## A. Simulation checklist (all platforms)

Record the OS, desktop/session type, Rust version and commit for each run.

| # | Steps | Expected |
|---|---|---|
| A1 | Start the app with an empty config dir | Window opens in **Simulation mode**; one mock remote; mock PowerPoint selected automatically; "Ready to route". `config.toml` is created. |
| A2 | Dashboard → *Start Routing* | Status "Routing active (simulated)"; sidebar dot turns green. |
| A3 | Input Devices → test panel → `forward` *Click* | Log shows **Input**, **Routing** ("Route Next → …") and **Delivery** ("simulated delivery") entries. Target Application shows "received 1 action(s)". |
| A4 | `forward` *Press* then *Release* | Press routed; release logged as "Ignored". |
| A5 | *Resend last event (duplicate)* | Rejected: duplicate event. Nothing delivered. |
| A6 | Uncheck *Enabled* on the device, click a button | Input logged, routing rejected ("device is disabled"). |
| A7 | *Disconnect*, then try the test buttons | Buttons disabled; dashboard warning "… is disconnected". |
| A8 | *Add mock device* (two-button clicker) and use both | Events from both devices route; IDs `mock-1`, `mock-2`. |
| A9 | Rename a device; restart the app | Name, enablement and devices persist. |
| A10 | Targets → *Acrobat*, then *Change target…* | Dialog lists PowerPoint and Acrobat. Routing paused; events rejected ("ambiguous"). |
| A11 | In the dialog: *Cancel* / *Pause routing* / *Choose another window…* | Cancel restores the previous target; Pause stops routing; Choose another opens the Targets page. |
| A12 | *Close* the selected window | Routing paused, target "closed; waiting…"; events rejected; the other application receives nothing. |
| A13 | *Restart* the target's process | Rebound to the new PID automatically; routing resumes. |
| A14 | Launch a 2nd and 3rd PowerPoint, then *Quit* the selected one | Dialog asks which of the identical windows to use. |
| A15 | Selection mode *Process*; *Select* PowerPoint with 2 windows | Dialog asks for the window; closing it moves to the other window of the same process. |
| A16 | Uncheck *Lifecycle notifications*, close the target window, click a button | Decision "Route…" followed by **delivery failed**; target becomes unavailable. *Refresh* updates the lists. |
| A17 | Settings: change each policy, mapping and log level; restart | All values persist. A custom action name is shown in the log on routing. |
| A18 | Settings: enter `Q` as shortcut → *Save* | Warning in the log; value snaps back. |
| A19 | Settings → *Reset configuration…* → *Reset to defaults* | Defaults restored and saved; extra mock devices removed. |
| A20 | Corrupt `config.toml` by hand; start | Warning on the dashboard; file kept as `config.toml.invalid-<time>`; defaults used. |
| A21 | Resize to the minimum (900×600) and maximize | No overlapping or clipped controls; lists scroll. |
| A22 | Switch the OS between light and dark themes | Text and status colors stay readable. |
| A23 | Diagnostics | Native capabilities show "Not implemented"/"Unsupported" with details; Native input mode cannot be selected. |
| A24 | Close the window | Process exits cleanly; config saved. |

## B. Arch Linux — X11

1. Run A1–A24 in an X11 session (`echo $XDG_SESSION_TYPE` → `x11`).
2. Diagnostics: environment shows "X11 session". Native targets are listed
   as *Not implemented* with EWMH/XSendEvent notes.
3. Check the `input` group note (`id -nG`) matches the diagnostics text.

## C. Arch Linux — Wayland

1. Run A1–A24 in a Wayland session (GNOME, KDE, or a wlroots compositor).
2. Diagnostics: "Wayland session (<desktop>)"; window discovery, lifecycle
   and background delivery are *Unsupported* with explanations.
3. Optionally repeat A1–A3 with `SLINT_BACKEND=winit-software` and with
   `WAYLAND_DISPLAY=` (XWayland) to rule out renderer issues.

## D. Windows 10/11

1. `cargo run` from a normal (non-administrator) terminal.
2. Run A1–A24. Configuration is under `%APPDATA%\RustClick\config\`.
3. Diagnostics list Raw Input / hook / EnumWindows plans as *Not implemented*.
4. Release build (`cargo build --release`): no console window opens.

## E. macOS

1. `cargo run`. Run A1–A24. Configuration is under
   `~/Library/Application Support/RustClick/`.
2. No permission prompts may appear (nothing native is used yet).
3. Diagnostics list IOHIDManager / CGEventTap and the required permissions as
   *Not implemented*.

## F. Native integration (future)

Once a native adapter exists, use the acceptance tests in
[native-input.md](native-input.md#acceptance-tests-before-claiming-native-support-per-platform)
with physical R500s and Spotlight remotes, for each connection type.

# Native integration: plan and limitations

On **Windows**, a first adapter exists in `src/platform/windows/` and is
marked *Experimental*: Raw Input capture of Logitech devices, window
discovery with `EnumWindows` and polling, and delivery through posted key
messages. Input suppression is not implemented. `rustclick-diag.exe`
(`src/bin/rustclick-diag.rs`) records what the hardware and applications
actually do; see [windows-testing.md](windows-testing.md). On Linux and
macOS, nothing in this document is implemented yet: the native providers
(`src/input/native.rs`, `src/targets/native.rs`) refuse to start. Every
platform reports its status through `src/platform/`, which is what
*Settings → Diagnostics* shows. This document records what each platform offers, what it
does not, and what must be verified on real hardware before RustClick may
claim support.

## Three separate problems

Keep these apart. Solving one does not solve the others.

1. **Detection and identification.** Seeing a key event *and* knowing which
   physical device produced it.
2. **Suppression.** Stopping the original keystroke (e.g. Page Down) from also
   reaching the focused application.
3. **Background delivery.** Making a *non-focused* application perform an
   action.

The core already treats them separately: input providers produce normalized
`InputEvent`s (1), providers grab devices in `set_device_enabled` (2), and
`TargetProvider::deliver` performs and reports delivery (3).

## Adapter contract

A native **input provider** must:

* Report every device with a stable `DeviceId` (e.g. derived from bus,
  vendor id, product id and serial or physical path) and `DeviceKind::Physical`.
* Never start capturing or grabbing a device until `set_device_enabled(id, true)`
  is called. The service only calls this after an explicit user action.
* Translate platform key codes or HID usages into `ButtonId`s through
  `normalize_button`, put the raw code in `InputEvent::metadata`, and number
  events per device with strictly increasing `sequence` values.
* Set `captured_at` at capture time, not when the event is processed.
* Send through `ProviderSink` without blocking. A full queue drops the event.

A native **target provider** must:

* Report processes with a `ProcessKey` whose `instance` is the process start
  time, and an `AppIdentity` containing the executable path and/or bundle id.
* Push `TargetsChanged` on window or process lifecycle changes if it can, and
  report `notifications_active() == false` otherwise.
* In `deliver`, re-validate that the window still exists and belongs to the
  same process lifetime, and return `Delivered` only if the platform
  confirmed the action. `Unsupported` and `Failed` must be honest.

## Hardware notes: Logitech R500s and Spotlight

Both remotes use Logitech's USB vendor id `0x046D` and present themselves to
the OS as HID devices (keyboard and possibly mouse interfaces) through a USB
receiver or Bluetooth LE. The exact product ids, interfaces and key codes per
button **must be recorded on real hardware** before any mapping is written.
Do not assume them:

* Linux: `evtest`, `libinput debug-events`, `udevadm info /dev/input/eventN`
* Windows: a Raw Input logger (`GetRawInputDeviceInfo`, `RIDI_DEVICENAME`)
* macOS: `hidutil list`, IOHIDManager logging

Questions to answer per remote and connection type (receiver vs. Bluetooth):
Which interface carries the buttons? Which keys does each press, hold and
double press emit? Does the Spotlight emit anything for its pointer or gyro
features without Logitech's software, or does that need its vendor HID++
protocol? (Vendor protocols are out of scope for the MVP.)

## Windows (10/11)

| Need | API | Notes |
|---|---|---|
| Identify device | Raw Input (`RegisterRawInputDevices` with `RIDEV_INPUTSINK`, `GetRawInputDeviceInfo`) | Reports the source device handle and HID path. No elevation required. |
| Suppress original keys | `WH_KEYBOARD_LL` hook | **Raw Input cannot block keys**, and the low-level hook **does not know the source device**. The two must be correlated (Raw Input arrives before the hook for the same key) and the timing validated on hardware. An Interception-style filter driver would be reliable but is a kernel driver, which is excluded. |
| Discover windows | `EnumWindows`, `GetWindowThreadProcessId`, `QueryFullProcessImageNameW`, `GetProcessTimes` (start time → `instance`) | |
| Lifecycle | `SetWinEventHook` (`EVENT_OBJECT_CREATE` / `DESTROY` / `NAMECHANGE`) | Out-of-context hooks need a message loop on the hook thread. |
| Background delivery | `PostMessage(WM_KEYDOWN/WM_KEYUP)`; PowerPoint COM automation (`SlideShowWindow.View.Next`) | Posted key messages are ignored by some applications; behavior must be verified for PowerPoint and Acrobat versions in use. UIPI blocks posting to elevated processes from a non-elevated RustClick. |
| Global shortcut | `RegisterHotKey` | |

Elevation: none for the above, except delivering to elevated target processes.

**Suppression through HID++.** Logitech remotes expose the vendor protocol
HID++ 2.0 on a separate HID collection (usage page `0xFF00`/usage 2 behind a
receiver, `0xFF43`/`0x0202` over Bluetooth LE). Its `REPROG_CONTROLS_V4`
feature (`0x1B04`) can *divert* a button: the remote then stops sending the
keystroke and reports the press as a HID++ notification. This solves
identification and suppression together, without hooks or drivers, for
remotes that support it. `rustclick-diag` lists each remote's controls, tries
diversion temporarily and records the notifications, so the input provider
can rely on measured control ids instead of assumptions.

## macOS

| Need | API | Permission |
|---|---|---|
| Identify and capture device | `IOHIDManager` with matching on vendor and product id | **Input Monitoring** |
| Suppress original keys | `CGEventTap` (can return `NULL` to drop) or seizing the HID device (`kIOHIDOptionsTypeSeizeDevice`) | **Accessibility** (event tap); seizing may require Input Monitoring |
| Discover apps and windows | `NSWorkspace.runningApplications` (bundle id, launch date → `instance`), `CGWindowListCopyWindowInfo` | Window titles require **Screen Recording** (10.15+) |
| Lifecycle | `NSWorkspace` launch and terminate notifications; `AXObserver` for windows | Accessibility |
| Background delivery | `CGEventPostToPid`, AppleScript / Apple Events for PowerPoint | Accessibility; Automation consent per target app |
| Global shortcut | Carbon `RegisterEventHotKey` | none |

The UI must show each missing permission with instructions
(`CapabilityStatus::PermissionRequired`). Distribution needs code signing and
notarization, or the permission prompts will not stick.

## Linux

| Need | Mechanism | Notes |
|---|---|---|
| Identify and capture device | evdev (`/dev/input/event*`), udev properties for vendor and product ids | Needs read access: membership in the `input` group or a udev rule (`TAG+="uaccess"`) for the remote's ids. Works under X11 and Wayland because it bypasses the display server. |
| Suppress original keys | `EVIOCGRAB` on the remote's event node | Grabs the whole node exclusively, so only the remote's keys disappear and the laptop keyboard is unaffected. |
| Discover windows (X11) | EWMH `_NET_CLIENT_LIST`, `_NET_WM_PID`, `/proc/<pid>/exe`, `/proc/<pid>/stat` start time | |
| Lifecycle (X11) | `PropertyNotify` on the root window | |
| Background delivery (X11) | `XSendEvent` (flagged `send_event`, ignored by many toolkits), XTEST (focused window only) | Must be verified per application. |
| Wayland | — | Regular clients **cannot** enumerate or address other clients' windows, and there is no standard protocol to send input to an unfocused window. Compositor-specific protocols (e.g. `wlr-foreign-toplevel-management`, KDE or GNOME extensions) could provide discovery on some desktops. `uinput` injection only reaches the focused window. Global shortcuts need the xdg-desktop-portal `GlobalShortcuts` interface. |

On Wayland, the realistic native target is therefore limited, and RustClick
reports window discovery and background delivery as `Unsupported` there.
PowerPoint and Acrobat are not native Linux applications anyway; Linux
targets would be e.g. LibreOffice Impress or PDF viewers, possibly controlled
through their own automation interfaces (UNO, D-Bus).

## Acceptance tests before claiming native support (per platform)

1. Each remote is listed with a stable id across reconnects and reboots, and
   starts **disabled**.
2. Enabling the remote captures its buttons. The laptop's own keyboard is
   never captured.
3. With the remote enabled and routing stopped, its original keystrokes
   behave as configured (suppressed or passed through), and that is documented.
4. With routing active and a *different* application focused, PowerPoint
   advances in the background. The delivery outcome is `Delivered` only when
   the slide actually changed.
5. Closing PowerPoint, the Acrobat-and-PowerPoint ambiguity, and restarts
   behave exactly as in simulation (same integration scenarios, run manually).
6. Unplugging the remote mid-presentation produces a visible warning and no
   stray input.
7. Permission denial (macOS) or missing device access (Linux) is shown with
   instructions, and nothing claims to work.

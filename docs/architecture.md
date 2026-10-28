# Architecture

## Layers

```text
┌──────────────────────────── binary (src/main.rs) ────────────────────────────┐
│ ui/*.slint  ── AppState global: properties + callbacks                        │
│ controller.rs ── callbacks → Command;  ViewModel → Slint properties           │
└───────────────┬───────────────────────────────────────────────▲──────────────┘
                │ Command (bounded queue)                        │ ViewModel
┌───────────────▼───────────────────────────────────────────────┴──────────────┐
│ app.rs ── AppService (one worker thread)                                      │
│   owns Config, DeviceRegistry, TargetSelection, Router, providers             │
└──────┬─────────────────────────────┬─────────────────────────────┬───────────┘
       │                             │                             │
┌──────▼───────────┐   ┌─────────────▼───────────┐   ┌─────────────▼───────────┐
│ core/            │   │ input/                  │   │ targets/                │
│ devices, input,  │   │ InputProvider trait     │   │ TargetProvider trait    │
│ mappings,        │   │  mock.rs   (working)    │   │  mock.rs   (working)    │
│ targets, routing │   │  native.rs (reports     │   │  native.rs (reports     │
│ config,          │   │   "not implemented")    │   │   "not implemented")    │
│ capabilities     │   └─────────────────────────┘   └─────────────────────────┘
│ (no OS, no GUI)  │   platform/{linux,windows,macos}.rs: capability reports
└──────────────────┘   storage.rs: config file I/O · logging.rs: tracing setup
```

* **`core`** is platform-independent. It never calls an OS API, a GUI toolkit
  or a device. Every rule about devices, mappings, ambiguity, lifecycle and
  routing lives here and is unit tested with plain data.
* **Providers** (`input`, `targets`) adapt a source of devices/events or
  applications/windows to the core model. Each one reports its
  [capabilities](../src/core/capabilities.rs) explicitly: `Supported`,
  `Unsupported`, `Permission required`, `Not implemented`,
  `Temporarily unavailable` or `Error`.
* **`AppService`** owns all state and providers. It is UI-independent, so the
  integration tests drive it directly and check the `ViewModel` it produces.
* **`controller` + `ui`** form the only Slint-aware code. The `.slint` files
  contain layout and presentation only. They call callbacks and never decide
  anything about routing.

The library crate contains everything except `main.rs`, so tests and the
`screenshots` example can use the real UI and service.

## Threading and event flow

There are two threads:

* **UI thread**: runs the Slint event loop. Callbacks enqueue a `Command` with
  `try_send`, so the UI never blocks.
* **Service thread**: `AppService::run` processes `Command`s and provider
  messages from **one bounded queue** (`QUEUE_CAPACITY` = 256) in arrival
  order. After each batch it publishes a `ViewModel` via
  `slint::Weak::upgrade_in_event_loop`, which Slint provides for cross-thread
  UI updates. The controller only replaces list models that changed.

Providers push `ProviderMessage`s (device added/removed, input event, targets
changed, error) through a non-blocking `ProviderSink`. When the queue is full,
the message is dropped, counted and surfaced as a warning. Messages never
accumulate without bound. Native providers will run their own capture threads
and use the same sink.

There is no async runtime. Everything is driven by blocking `recv` on the
service thread: no polling and no busy waiting.

**Shutdown:** when the window closes, `main` sends `Msg::Shutdown` (a blocking
send, after the event loop has ended). The service stops all providers, saves
the configuration and returns; `main` joins the thread. Once the event loop
has stopped, further `upgrade_in_event_loop` calls simply fail and are ignored.

Because target changes and event routing run serially on the service
thread, a target change can never interleave with the routing of an event.

### Life of an input event

```text
provider ──Input(event)──▶ AppService::on_input
   1. log the input event                              (category "Input")
   2. Router::route(event, context) → Decision         (core, pure)
        duplicate? → source device known/enabled/connected/has button?
        release → Ignore;  routing stopped?  stale?  mapped?
        target: none / unavailable / awaiting confirmation → Reject
        bound window still in snapshot?  simulated event → native target?
   3. log the decision                                 (category "Routing")
   4. if Route: TargetProvider::deliver(target, action)
        the provider re-validates the destination against live state
        → Simulated | Delivered | Unsupported | Failed
   5. log the delivery outcome separately              (category "Delivery")
        Failed → refresh the snapshot and reconcile the target
```

Routing decisions and delivery outcomes are always separate entries. A
successful decision is never reported as a successful delivery.

## Target model and lifecycle

* `TargetSnapshot`: what a provider currently sees (processes and windows).
* `ProcessKey { pid, instance }`: one process *lifetime*. `instance` is the
  platform start time (a counter in the mock), so a restarted process or a
  reused PID is never confused with the original.
* `AppIdentity`: name, process name, executable path, bundle id.
  `same_app` compares **only** bundle id or executable path. An application
  without either is never matched again after it closes.
* `TargetCriteria { mode, identity, window_title }`: what is persisted.
  It holds no PIDs and no window handles.
* `TargetSelection`: the state machine in [`core/targets.rs`](../src/core/targets.rs).

```text
                 select / confirm
  NoSelection ─────────────────────────▶ Bound(process, window)
      │  1 eligible + auto-select            │  window gone / process exited
      │  ≥2 eligible → prompt                ▼
      ▼                               recover(criteria)
  AwaitingConfirmation ◀── ambiguous ──┤  0 matches → Unavailable(reason)
      │ confirm → Bound                │  1 match + AutoRebind → Bound
      │ cancel  → previous or          │  1 match + Ask → AwaitingConfirmation
      │           Unavailable/None     └─ ≥2 matches → AwaitingConfirmation
```

Replacement rules:

* **Window mode:** a replacement must have the same stable identity and the
  same window title.
* **Process mode:** another window of the same process is preferred. If the
  process exited, windows of any process with the same stable identity are
  considered (restart detection).
* With several candidates, or with the "always ask" policy, the user is asked.
  RustClick never picks one arbitrarily.
* A prompt the user dismissed is not reopened for the same situation. A new
  eligible window, an explicit *Change target*, or a manual selection
  re-enables prompts.
* Every status change bumps a generation counter and timestamp. Events
  captured before the last change are rejected as stale, so input from before
  a target switch never reaches the new target.

## Safety invariants and where they are enforced

| Invariant | Enforced in |
|---|---|
| Never route to an arbitrary or foreground application | `Router::route` routes only to `TargetStatus::Bound`; nothing falls back to focus |
| Never route while ambiguous | `TargetStatus::AwaitingConfirmation` → `RejectReason::TargetAmbiguous` |
| Never send simulated events to native applications | `RejectReason::SimulatedToNative`; mock delivery refuses native targets |
| Never silently enable a physical device | `DeviceKind::enabled_by_default`; physical devices start disabled unless the user enabled them before |
| Validate the destination right before delivery | Router checks the snapshot; `TargetProvider::deliver` re-checks live state |
| No duplicate delivery | Per-device sequence numbers in `Router`; release events are ignored |
| No stale delivery after a target change | `captured_at < selection.changed_at()` or older than 2 s → rejected |
| PIDs and handles are never persistent identity | `TargetCriteria` has no PID or handle; `same_app` uses only stable ids |
| Native features are never shown as working | Capability reports; native mode is selectable only if every capability is `Supported` |

## Configuration

`core/config.rs` defines a versioned (`version = 1`) TOML document with serde
defaults for every field. `Config::validate` reports all problems at once.
`storage.rs` writes atomically (temporary file, then rename). A file that
does not parse or validate is renamed to `config.toml.invalid-<unix time>`,
and defaults are used with a visible warning. Simulated devices are persisted
with their profile so they survive restarts. Physical devices keep only their
enablement and alias.

Switching between simulation and native mode clears the saved target, so
criteria picked among mock windows can never bind a real application.

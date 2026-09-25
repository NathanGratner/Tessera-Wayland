# Tessera — Architecture

How the pieces fit together, and where each responsibility lives. For how to run it and every key binding, see [`USAGE.md`](USAGE.md).

---

## The shape of the system

Two main processes, a small command-line tool, four libraries:

```
  tessera-launcher                      tessera-comp
  ┌────────────────────┐   wayland      ┌──────────────────────────────┐
  │ app: screen stack  │ ─────────────▶ │ handlers/  protocol glue     │
  │ ui:  menuconfig    │                │ layout/    tiling + placement│
  │ frontend/tty       │                │ input.rs   bindings + focus  │
  │ frontend/wayland ──┼─── ipc socket ─▶│ ipc.rs     requests, events  │
  │ worker (threads)   │◀── events ──────│ scripts/   run user scripts  │
  └───┬──────────┬─────┘                └──────────────────────────────┘
      │ draws    │ systemctl
┌─────▼───────┐ ┌▼─────────────────┐     tessera-serv (CLI) ── systemctl
│tessera-cells│ │ tessera-services │◀───  same library, same services.toml
└─────────────┘ └──────────────────┘
  other clients (foot, alacritty, …) ──── wayland ────▶ tessera-comp
```

The launcher is an ordinary Wayland client. It gets no special treatment beyond two key bindings and a private socket for requests Wayland deliberately cannot express (such as "open this program next to me").

### The IPC socket (`comp/ipc.rs`, `tessera-ipc`)

`$XDG_RUNTIME_DIR/tessera-<display>.sock`, mode 0600, one JSON object per line. Children of the compositor find it through `TESSERA_SOCKET`.

The interesting part is that **the caller never names its own window**. `Placement::BesideCaller` is resolved by reading the connecting process's id from the socket's peer credentials and finding the window whose client is that process, or one of its ancestors — so a script running inside a terminal anchors on that terminal.

Spawning and placing are two separate events, so the compositor records a `PendingPlacement` (pid, activation token, what was asked for, a 3 second deadline) and matches the window when it maps: activation token first, then pid or a descendant, then the oldest waiting spawn. That last fallback is what makes single-instance terminals work, where the window is created by a daemon the compositor never started.

Most connections are one request, one answer. A client that sends `Subscribe` keeps its connection and receives every `Event` as a line (`ScriptStarted`, `ScriptExited`, `ScriptsChanged`); the compositor keeps those streams in `Tessera::subscribers` and drops any that cannot take a line.

---

## `tessera-comp` — the compositor

One thread, one [calloop](https://docs.rs/calloop) event loop, one big state struct.

### Event sources

| Source | Fires when | Registered in |
|--------|-----------|---------------|
| Wayland listening socket | A client connects | `state.rs` |
| Wayland display | A client sends requests | `state.rs` |
| winit backend | Input, resize, redraw, window closed | `backend/winit.rs` |
| IPC listening socket | The launcher or a script connects | `ipc.rs` |
| IPC connection (one per client) | A request arrives | `ipc.rs` |
| inotify on the scripts folder | A script is added, removed, edited or `chmod`ed | `scripts/watch.rs` |
| Timer, 200 ms | A burst of folder changes settled: rescan once | `scripts/mod.rs` |
| Script stdout / stderr pipe | A running script printed something, or closed its output | `scripts/supervisor.rs` |
| Script pidfd | A running script exited | `scripts/supervisor.rs` |
| Timer, 3 s | A script asked to stop has not: SIGKILL | `scripts/supervisor.rs` |
| DRM device (udev backend) | A frame reached the screen (vblank) | `backend/udev.rs` |
| udev monitor (udev backend) | A monitor was plugged in or out | `backend/udev.rs` |
| libinput (udev backend) | Real keyboards, mice, touchpads | `backend/udev.rs` |
| Session notifier (udev backend) | A VT switch takes the screen away, or gives it back | `backend/udev.rs` |

After **every** dispatch, the loop callback in `main.rs` refreshes the space, cleans up popups and flushes clients. That happens outside the redraw path on purpose: clients must keep making progress even when nothing is being drawn (see NOTES, S2).

### State

`state.rs` holds `Tessera`: the Smithay protocol states, the `Space` of mapped windows, nine `LayoutTree`s, which workspace is active, the focused window, the layer surface holding the keyboard (if any), the binding table, the script list and the event subscribers. Smithay's `delegate_*!` macros connect each protocol to a handler implemented on this type.

### Handlers (`handlers/`)

| File | Protocol | Notable behaviour |
|------|----------|-------------------|
| `compositor.rs` | `wl_compositor` | On commit: buffer bookkeeping, then initial configures (popups and layer surfaces) |
| `xdg_shell.rs` | `xdg_shell` | New toplevel → tile it and focus it; destroyed → untile and focus a neighbour |
| `layer_shell.rs` | `wlr-layer-shell` | Overlays, bars, launchers: placed by each output's `LayerMap`, never tiled. Decides which layer surface holds the keyboard (below) |
| `decoration.rs` | `xdg-decoration` | Always answers **server side**, so tiles have no client title bars |
| `seat.rs` | `wl_seat`, `wl_data_device` | Keyboard/pointer focus, clipboard follows keyboard focus |
| `shm.rs` | `wl_shm` | Shared-memory buffers, which the launcher uses |
| `output.rs` | `wl_output` | Output advertisement |
| `activation.rs` | `xdg-activation` | Records the token a client presents, so spawns can be matched to windows |
| `dmabuf.rs` | `linux-dmabuf` | Only with the udev backend: asks the renderer whether a GPU buffer can be imported |

### Tiling (`layout/`)

`layout/tree.rs` is a **pure** binary split tree: leaves are windows, inner nodes split their area along an axis at a ratio. It contains no Smithay types and is generic over the window id, so it is unit-testable anywhere — 24 of the compositor's tests cover it alone.

```
LayoutTree                        screen
└── Split { Horizontal, 0.5 }     ┌─────────┬─────────┐
    ├── Leaf(A)                   │         │    B    │
    └── Split { Vertical, 0.5 }   │    A    ├─────────┤
        ├── Leaf(B)               │         │    C    │
        └── Leaf(C)               └─────────┴─────────┘
```

Operations: `insert` (dwindle or "beside"), `remove` (the sibling takes the space), `neighbor`, `swap`, `resize`, and `layout`, which turns the tree into rectangles with gaps and optional cell snapping.

`layout/place.rs` handles windows that were asked for over IPC: resolving a request into a `PendingPlacement`, matching a new window against those, and the window-id bookkeeping the IPC window list needs.

Two placement rules involve the launcher, both in `place.rs`: `launcher_placement` opens the launcher on its configured side of the focused window at `100 − share` percent, and `beside_launcher_or_auto` puts a terminal opened without a caller (Mod+Return, a key-bound terminal script) on the launcher's other side at `share` percent when the launcher is open on this workspace.

`layout/apply.rs` is the bridge to reality: it asks the tree for rectangles, then for each window sets the pending size, sends a configure and maps it into the `Space`. Workspace switching, moving windows between workspaces, focus movement and the launcher toggle live here too.

### Input (`input.rs`)

Key events pass through a filter before reaching the focused client. If the key matches the binding table, the compositor intercepts it and runs an `Action`; otherwise it is forwarded. Bindings are keyed on (modifiers, keysym) using the **unshifted** symbol, so `Mod+Shift+E` matches the `e` key on any layout.

**Focus has two parts.** `Tessera::focus` is the focused *window*: the one new windows tile beside and bindings act on. `Tessera::layer_focus` is a layer surface holding the keyboard instead, such as the application overlay. The keyboard goes to the layer surface when there is one, else to the window (`update_keyboard_focus`). Keeping them apart is what lets the overlay borrow the keyboard without Tessera forgetting the window: what it launches tiles beside that window, and the keyboard returns to it when the overlay closes. A layer surface asking for an `exclusive` keyboard on the overlay or top layer takes it while mapped (`refresh_layer_focus`); an `on_demand` one takes it when clicked. Clicking a window while an exclusive overlay is up focuses the window but leaves the keyboard with the overlay.

**Hit-testing** (`surface_under`) stacks the layers around the windows: overlay and top above them, bottom and background below. Focus-follows-mouse ignores the windows under an overlay or bar.

The table is rebuilt (`rebuild_bindings`) whenever the configuration is applied or the scripts folder is rescanned: fixed keys, then configured ones, then `Bindings::add_scripts`, which refuses any script key already taken and drops both keys when two scripts collide.

### Scripts (`scripts/`)

| File | Responsibility |
|------|----------------|
| `mod.rs` | `Scripts` state, rescans that keep each script's run state, binding merge, run requests (background or terminal) |
| `discover.rs` | Pure: scan a folder for executables and read their headers. Tested against temporary folders |
| `output.rs` | Pure: the 500-line buffer per script, line splitting across reads, colour codes stripped |
| `supervisor.rs` | Spawning with pipes in a new process group, pidfd exit detection, SIGTERM/SIGKILL stop |
| `watch.rs` | The inotify source |

A background run is identified by a run id, so a pipe still open from an old run (a grandchild holding it) can never write into a newer run's output, and a SIGKILL timer never hits a newer run.

### Backends (`backend/`)

Two backends, each with one job: get input in and pixels out.

| File | Used when | How it draws |
|------|-----------|--------------|
| `winit.rs` | Nested in another desktop | One output sized to the window, a damage tracker, `render_output` every frame |
| `udev.rs` | Tessera is the session | One `DrmOutput` per connected screen, drawn on the GPU through GBM and scanned out directly |

`udev.rs` follows anvil's backend, without its multi-GPU renderer, DRM leases
or syncobj support, and with its own 40-line connector scan in place of
`smithay-drm-extras` (which does not build at the pinned tag on current Arch).
It owns the `GlesRenderer`, the session (`LibSeatSession`, which also switches
VTs), libinput, and the `linux-dmabuf` global that lets clients render on the GPU.

Both backends draw layer surfaces without any code of their own: Smithay's
`space_render_elements` (and `render_output`, which uses it) includes each
output's layer map. Frame callbacks are not automatic, so both call
`Tessera::send_frames`, which covers windows **and** layer surfaces; a surface
left out would draw once and then wait forever. `output_area` is the screen
less the layer map's exclusive zones, so a bar is never tiled over.

Drawing is driven by the screen: render a frame, queue it, and on the vblank
draw the next. When a frame has no damage, a timer retries about one refresh
later, so an idle desktop costs nothing. A VT switch pauses the session: the
GPU and input devices go back, and are taken again on return.

---

## `tessera-config` — settings

The schema and its values, shared by both programs so they can never disagree about what a setting means.

| Module | Responsibility |
|--------|----------------|
| `schema.toml` | Every setting: kind, default, constraints, help, `depends_on`, `apply = live \| restart` |
| `schema.rs` | Parses and validates the schema; `check` validates a value and explains refusals |
| `values.rs` | `ConfigValues`: reads, edits and atomically saves `config.toml` through `toml_edit`, keeping comments |
| `expr.rs` | The `depends_on` language: `a`, `!a`, `a && b`, `a \|\| b`, parentheses |
| `binding.rs` | The shape of a binding string, `Mod+Shift+Return` |
| `scripts.rs` | The scripts folder, `# tessera:` header parsing, and rewriting the `autostart` line; shared so compositor and launcher agree on the format |

In the compositor, `settings.rs` turns a `ConfigValues` into running state (`apply_config`, all or nothing) and answers `ReloadConfig`. In the launcher, `config_menu.rs` generates the menuconfig screens from the schema.

---

## `tessera-services` — systemd services

A library used by the launcher, and the `tessera-serv` binary built from the same crate.

| Module | Responsibility |
|--------|----------------|
| `lib.rs` | `Service` (unit + system/user `Scope`), unit name normalisation (`sshd` → `sshd.service`) |
| `list.rs` | `services.toml`, edited with `toml_edit` so comments survive additions and removals |
| `systemctl.rs` | Batch state queries (`systemctl show`), actions with `--no-ask-password`, recognising "a password is needed", `status` text |
| `main.rs` | `tessera-serv`: runs systemctl attached to the terminal, then updates the list |

The compositor does not know services exist: systemd supervises them, nothing needs a Tessera key binding for them, and `systemctl` can block, which the compositor's loop must never do.

---

## `tessera-cells` — the character grid

The launcher's renderer, and the part of Tessera that makes it look like a TUI without being one.

```
ratatui widgets → CellBackend → Grid → paint() → Argb8888 pixels → wl_shm buffer
                                  ▲
                              Font (fontdb + swash) ── glyph atlas
```

| Module | Responsibility |
|--------|----------------|
| `grid.rs` | Cells (character, colours, bold/underline) with per-cell dirty tracking |
| `font.rs` | Font lookup, cell metrics (width, height, baseline, underline), cached glyph rasterisation |
| `boxdraw.rs` | Classifies box-drawing and block characters into geometry |
| `paint.rs` | Paints dirty cells into a pixel buffer and reports damage rectangles |
| `backend.rs` | Implements ratatui's `Backend` trait over the grid |
| `color.rs` | `Rgb`, blending, and the palette named colours resolve through |

Two details worth knowing:

- **Box-drawing characters are drawn, not typeset.** Fonts round these strokes and rarely align between cells, so `paint.rs` draws each arm from the cell centre to the cell edge. Neighbouring cells therefore always meet.
- **Only changed cells are repainted**, and the painter returns one damage rectangle per run of changed cells in a row. The pixels beyond the last whole cell are filled too, or they show whatever the buffer previously held.

---

## `tessera-launcher` — the launcher

One app core, two front ends.

```
        ┌── frontend/tty.rs      (crossterm; any terminal, ssh included)
App ────┤
        └── frontend/wayland.rs  (SCTK window or layer surface + tessera-cells)
```

The Wayland front end draws into one of two surfaces (`Shell`): an `xdg_toplevel`, tiled like any window, for the full launcher; or with `--apps`, a layer surface on the overlay layer (namespace `tessera.apps`, no anchors so it is centred, exclusive keyboard), sized to about 64 × 22 cells and fitted to the screen once the output reports its size. Both kinds of configure feed the same resize and draw path.

Both translate their input into the same neutral `event::Event`, so `app.rs` never knows which one is running. That is what lets the UI be developed and tested in a terminal, and unit-tested headlessly with ratatui's `TestBackend`.

- `app.rs` — a stack of screens (`Main`, `Apps`, `Message`), `update(Event) -> Vec<Effect>` and `view(&mut Frame)`. Effects are things only the front end can do: `Spawn { argv, placement }` and `Quit`. `App::apps_only` is the overlay's mode: the stack starts on `Apps` with the filter on, leaving it quits, launching quits after the spawn, and the dialog fills the surface instead of sitting on the blue screen.
- `config_menu.rs` — the configuration screens: menus, input box, choice list, search and help, all generated from the schema. Holds edits between visits.
- `power_menu.rs` — Power & session: log out (a `Quit` request to the compositor) and suspend, reboot, power off (through `systemctl` on the worker). Every choice asks first, with No selected.
- `scripts_menu.rs` — Scripts & services: both lists, the output and status box, the add-a-service box. Pure like the app core: keys and results in, effects out.
- `worker.rs` — work that must not block the loop, on threads: the compositor's event stream, every `systemctl` call, and a one-second tick. Results come back as `Update`s through a channel each front end watches (a calloop channel in the window, an mpsc channel polled in `--tty`), and go to `App::background`.
- `ipc.rs` — the client side of the compositor socket. Under Tessera, spawning goes through the compositor so it can place the window; elsewhere the launcher runs the program itself and says so in its status line.
- `ui.rs` — the menuconfig look: blue ground, grey dialog with its title on the top border, red hotkey letters, drop shadow, button row.
- `apps.rs` — the `.desktop` catalogue and fuzzy search.
- `event.rs` — the front-end-independent key, mouse and resize types.

---

## What isn't built yet

| Piece | Where it would go |
|-------|-------------------|
| Tiling across more than one screen (today the first screen tiles, the rest show the background) | `layout/apply.rs`: `output_area` assumes one screen |
| Cursor themes and client cursor shapes (today a grey square) | `backend/udev.rs`, `handlers/seat.rs` (`cursor_image`) |
| Fractional scaling | `backend/*`, and the launcher's cell metrics |
| Xwayland | out of scope for v0.1 |

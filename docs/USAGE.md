# Tessera — Usage

Running Tessera today, and every key it responds to.

Tessera runs two ways: **nested**, as a window inside your existing desktop, which is how it is developed; and as **your session**, driving the hardware from a TTY or a display manager.

---

## Build and run

```bash
cargo build                                    # tessera-comp, tessera-launcher, tessera-serv
cargo run -p tessera-comp -- --nested           # empty compositor
cargo run -p tessera-comp -- --nested --spawn foot
```

System packages are listed per distro in [`DEPENDENCIES.md`](DEPENDENCIES.md). The launcher needs a monospace font; JetBrains Mono is the default.

### `tessera-comp` options

| Option | Effect |
|--------|--------|
| `--backend auto` | A window if a desktop session is running, the hardware otherwise. The default |
| `--backend winit` | A window inside the current desktop (`--nested` means the same) |
| `--backend udev` | The hardware directly, from a TTY: Tessera is the session |
| `--spawn <command>` | Run a shell command as a client once the compositor is up. Repeatable |
| `-h`, `--help` | Show usage |

Asking for `udev` from inside a running desktop is refused: two compositors
cannot both drive the screen, and the result would be a black one. Switch to a
TTY first. `TESSERA_FORCE_UDEV=1` overrides the refusal, and
`TESSERA_DRM_DEVICE=/dev/dri/card1` picks a GPU when the automatic choice is wrong.

Logging is controlled by `RUST_LOG`: `RUST_LOG=tessera_comp=debug` also prints every tile rectangle, which is the quickest way to check tiling without looking at the screen.

The launcher is found next to the compositor binary first, so a freshly built
pair in `target/debug` works with no setup; failing that, it is looked up on `PATH`.

### `tessera-launcher` options

| Option | Effect |
|--------|--------|
| `--tty` | Run in the current terminal instead of opening a window |
| `--font <family>` | Font for the window front end (default: JetBrains Mono) |
| `--font-size <px>` | Font size in pixels (default: 16) |

With no options it opens a window when `WAYLAND_DISPLAY` is set, and falls back to the terminal otherwise.

---

## Key bindings

**Mod is Alt** while running nested, because host desktops keep Super for themselves. When Tessera owns the session, Mod is **Super**.

### Windows

| Binding | Action |
|---------|--------|
| `Mod+H` / `J` / `K` / `L` | Move focus left, down, up, right |
| `Mod+Shift+H/J/K/L` | Swap the focused window with that neighbour |
| `Mod+Ctrl+H/J/K/L` | Move the nearest split edge in that direction, by 5% (limits: 10%–90%) |
| `Mod+Q` | Close the focused window |
| Click | Focus the window under the pointer |

### Workspaces

| Binding | Action |
|---------|--------|
| `Mod+1` … `Mod+9` | Switch to that workspace |
| `Mod+Shift+1` … `Mod+Shift+9` | Send the focused window there (you stay put) |

Workspaces are independent tiling trees. Hidden ones keep their layout.

### Session

| Binding | Action |
|---------|--------|
| `Mod+Return` | Open a terminal (the one set in Configuration → General). While the launcher is open on this workspace, the terminal opens beside it at the configured share, exactly as the launcher's Terminal entry does; otherwise it tiles the usual way |
| `Mod+Space` or `Mod+\` | Focus the launcher, or start it if it isn't running. It opens on its configured side of the focused window (Configuration → Launcher). Use `Mod+\` while nested: Plasma and GNOME keep Mod+Space for their own search |
| A script's `bind` | Run that script, whether or not the launcher is open (see Scripts & services) |
| `Mod+Shift+E` | Quit Tessera |
| `Ctrl+Alt+F1` … `F12` | Switch virtual terminal (when Tessera is the session). This is the way out if something goes wrong |

---

## How tiling behaves

The first window fills the screen. Each new window **splits the focused one along its longer side**, so windows dwindle into the space you are looking at:

```
  one            two                three (focus on B)
┌───────┐    ┌─────┬─────┐       ┌─────┬─────┐
│       │    │     │     │       │     │  B  │
│   A   │    │  A  │  B  │       │  A  ├─────┤
│       │    │     │     │       │     │  C  │
└───────┘    └─────┴─────┘       └─────┴─────┘
```

Closing a window gives its space back to its sibling, and focus moves to the nearest neighbour. Gaps default to 8 px; see Configuration.

---

## The launcher

Open it with `Mod+Space`, or run it in a terminal with `tessera-launcher --tty`.

| Key | Action |
|-----|--------|
| `↑` `↓`, or `j` `k` | Move the selection |
| Highlighted letter | Jump straight to that entry |
| `Enter` | Open the selected entry |
| `Esc` `Esc` | Go back one screen |
| `/`, or just type | Filter the application list |
| `?` | Help |
| `q` or `b` | Back, or quit from the main menu |
| `x` | Exit, from the main menu |
| Letters in the buttons | Work like any other hotkey, except on the application list where letters filter instead |

Holding a key repeats it only where that makes sense — moving the selection, paging, deleting filter text. Enter and Esc never repeat, so holding Enter cannot dismiss a dialog and activate whatever was underneath it.

The main menu has six entries: **Launch application**, **Terminal**, **Scripts & services**, **Configuration**, **Windows** and **Power & session**.

**Power & session** offers **Log out of Tessera**, **Suspend**, **Reboot** and
**Power off** (hotkeys `L`, `S`, `R`, `P`). Every one asks first, with **No**
highlighted, so Enter alone never does it: press `y`, or move to `<Yes>` with
the arrow keys. Log out asks the compositor to end the session; the others go
through logind, and if it wants a password they open a terminal beside the
launcher to ask. When Tessera is only a window inside another desktop, the
question says that suspending, rebooting or powering off affects the whole
computer. There is no **Lock** yet: locking needs the `ext-session-lock`
protocol, which Tessera does not implement.

**Where the launcher and its terminals go.** The launcher opens on one side of the focused window (Configuration → Launcher → *Launcher side*, left by default) and takes what the terminal share leaves: with the share at 60%, the launcher gets 40%. Terminals opened beside the launcher go on its *other* side and take the share, so the launcher keeps to its edge. The share is of the launcher's tile *at that moment*, so a second terminal takes its share of a launcher tile the first one already shrank.

**Windows** lists every window the compositor knows about, across all workspaces, with its workspace number, application and title; `*` marks the focused one. Enter switches to it — changing workspace if needed — and closes the launcher.

The application list reads `.desktop` files from `$XDG_DATA_DIRS/applications` and `~/.local/share/applications`. Typing filters them fuzzily; applications marked `Terminal=true` are launched inside a terminal.

---

## Running Tessera as your session

Install the binaries and the session files:

```bash
cargo build --release
sudo install -Dm755 target/release/tessera-comp     /usr/local/bin/tessera-comp
sudo install -Dm755 target/release/tessera-launcher /usr/local/bin/tessera-launcher
sudo install -Dm755 target/release/tessera-serv     /usr/local/bin/tessera-serv
sudo install -Dm755 session/tessera-session         /usr/local/bin/tessera-session
sudo install -Dm644 session/tessera.desktop         /usr/share/wayland-sessions/tessera.desktop
```

Then either pick **Tessera** in your display manager (greetd, GDM, SDDM), or
log in on a TTY and run:

```bash
tessera-session
```

`tessera-session` sets the session environment (`XDG_SESSION_TYPE`,
`XDG_CURRENT_DESKTOP`, the Wayland backends for Qt and GTK) and starts
`tessera-comp --backend udev`. The compositor tells `systemd --user` about
`WAYLAND_DISPLAY` once its socket exists, so portals and user services can find it.

**What you need:** a seat manager — systemd-logind, or seatd (`systemctl enable
--now seatd` and your user in the `seat` group) — and a kernel with DRM/KMS for
your GPU. No root, and no X11.

**The log** goes to `$XDG_STATE_HOME/tessera/tessera.log` (usually
`~/.local/state/tessera/tessera.log`) when nothing is attached to the terminal,
which is what a display manager gives you. From a TTY it prints as usual. Runs
are appended, so the log of a session that died survives the next start; past
2 MB the old one becomes `tessera.log.1`.

### What Tessera tells the programs it starts

Every program started by Tessera — from the launcher, a binding, or a script —
gets `WAYLAND_DISPLAY`, no `DISPLAY` at all, and the environment toolkits need
to pick their Wayland backends:

```
XDG_SESSION_TYPE=wayland   QT_QPA_PLATFORM=wayland   GDK_BACKEND=wayland
SDL_VIDEODRIVER=wayland    CLUTTER_BACKEND=wayland   MOZ_ENABLE_WAYLAND=1
```

This matters most for **Qt 5**, which otherwise defaults to X11 and simply
fails to start where there is no X server. The two theme settings above are
added when they are not empty.

### What works, and what does not yet

- Every screen that is plugged in gets a `wl_output`, laid out left to right in
  the order found. Plugging and unplugging is picked up.
- **Screens use their preferred mode unless you say otherwise.** For a 4K
  monitor that means 4K, which a laptop GPU may struggle to drive: Tessera
  composites the whole screen each frame, so 3840×2160 is four times the work
  of 1080p. Configuration → Screens → *Resolution* takes `1920x1080`, or
  `1920x1080@60` to pin the refresh rate too, and applies immediately. One
  setting applies to every screen; per-screen configuration comes later.
- **Windows are tiled on the first screen only.** The tiling model has one
  screen's worth of space; a second monitor shows the background. Multi-monitor
  tiling comes after v0.1.
- `Ctrl+Alt+F1`–`F12` switches VT; Tessera hands back the GPU and input, and
  takes them again when you switch back.
- GL clients can render on the GPU: `linux-dmabuf` is advertised here (nested
  Tessera still has them fall back to software).
- **The pointer is a plain light-grey square.** Cursor themes, and the shapes
  clients ask for, come later.

---

## Scripts & services

Press **`S`** in the launcher. One dialog, two lists.

### Scripts

A script is any executable file in `~/.config/tessera/scripts`. Drop one in (and `chmod +x` it) and it appears within a second, no restart. The compositor runs scripts, not the launcher, so key bindings and autostart work with the launcher closed, and a script outlives the launcher that started it.

An optional header in the first 20 lines describes it; the script runs fine without Tessera:

```sh
#!/bin/sh
# tessera: name = "Sync notes"
# tessera: description = "Push ~/notes to the remote and pull changes"
# tessera: mode = "background"        # background | terminal
# tessera: bind = "Mod+Shift+N"
# tessera: autostart = false
cd ~/notes && git pull --rebase && git push
```

| Key | Action |
|-----|--------|
| `Enter`, `R` | Run it in the background, capturing its output |
| `T` | Run it in a terminal tiled beside the launcher |
| `E` | Edit it (Configuration → General → *Editor*, nano by default) in a tiled terminal |
| `O` | Show the output of its last run; follows live while it runs. stderr is red |
| `K` | Kill it: SIGTERM to it and everything it started, then SIGKILL after 3 seconds |
| `A` | Toggle `[*]`: run when Tessera starts. This rewrites the header line in the file |

Each row shows `[*]` for autostart, the name, the state (`never run`, `running` with a timer, `ok`, `exit 1`, `SIGTERM`, `terminal`), when, and the binding. `(!)` marks a problem — a header line that could not be read, or a binding that clashes — and the reason shows when the script is selected. A binding never takes a key from Tessera itself: one that clashes with a built-in or configured binding is ignored, and two scripts claiming the same key both lose it. Scripts get `TESSERA_SOCKET` and `TESSERA_SCRIPT` (their file name) in their environment and run from your home directory.

### Services

systemd units you choose to see here, from the system manager or your own (`--user`). Tessera keeps only the list, in `~/.config/tessera/services.toml`; systemd does the rest.

| Key | Action |
|-----|--------|
| `Enter`, `Space` | Start it if it is stopped, stop it if it is running |
| `R` / `K` | Restart / stop |
| `S`, `O` | Show `systemctl status` in a Tessera box, refreshed every two seconds |
| `A` | Toggle `[*]`: start at boot (`systemctl enable` / `disable`). `-` means a static unit, which cannot be |
| `N` | Add a service: type `sshd`, or `--user syncthing` |
| `D` | Remove it from this list. The service itself is not touched |

**Passwords.** Changing a system service usually needs one. The launcher first tries without asking; if systemd says a password is required, it opens the same action in a terminal tiled beside the launcher, where systemctl asks for it. User services never need one.

### `tessera-serv`

The same, from any shell. It runs the systemctl command attached to your terminal (so it can ask for a password there) and keeps the unit in Tessera's list:

```bash
tessera-serv enable sshd            # systemctl enable sshd.service, then list it
tessera-serv start --user syncthing # a user service
tessera-serv enable --now sshd      # enable and start
tessera-serv add bluetooth          # only list it
tessera-serv remove bluetooth       # only stop listing it
tessera-serv status sshd            # systemctl status
tessera-serv list                   # what Tessera shows, with states
```

`enable`, `start` and `restart` add the unit to the list; `disable` and `stop` leave the list alone. Unknown units are refused. When built from source it is `target/debug/tessera-serv`; the launcher finds it next to itself.

---

## Configuration

Press **`C`** in the launcher, or edit `~/.config/tessera/config.toml` by hand — both take the same path. The file only contains what you changed; everything else uses its default.

```toml
[general]
terminal = "alacritty"

[layout]
gaps = 12
snap_to_cells = true

[bindings]
terminal = "Mod+t"
```

### In the configuration menu

| Key | Action |
|-----|--------|
| `↑` `↓`, `Enter` | Move; open a submenu or edit the selected value |
| `Space`, `y`, `n` | Toggle an on/off setting |
| `?` or `h` | Explain the setting: what it does, allowed values, default, when it takes effect |
| `/` | Search every setting by name; `Enter` jumps to it |
| `z` | Show settings hidden because something they depend on is off (shown dimmed) |
| **`a`** | **Save** to config.toml and apply |
| `l` | Discard unsaved edits and re-read the file |
| `Esc`, `x` | Back |

The markers follow `make menuconfig`: `[*]` on, `[ ]` off, `(value)` a number or text, `--->` a submenu or a list of choices, `-*-` switched on by another setting. After saving, settings that cannot change while running are marked **`(restart)`** — the launcher font, for example, takes effect the next time the launcher opens. Everything else applies immediately.

A value that breaks a rule is refused where you typed it, with the rule: *"999 is outside the allowed range 0–64"*. If config.toml itself is broken (edited by hand, say), Tessera starts with defaults and the configuration menu says what is wrong.

### Bindings

Written as modifiers and a key joined with `+`: `Mod+Shift+Return`, `Mod+backslash`, `Mod+Ctrl+F1`. `Mod` is required, so Tessera never takes a key away from the program you are typing in. Key names are xkb's (`Return`, `space`, `backslash`, `Escape`, `F1`); single letters are case-insensitive. An unknown name is refused when you save.

Configurable: open the launcher (two bindings), open a terminal, close a window, quit. Moving focus, swapping, resizing and switching workspaces stay on `H/J/K/L` and `1–9`. Scripts bind their own keys in their headers.

### Other settings worth knowing

| Setting | Where | What it does |
|---------|-------|--------------|
| Editor | General | What `E` opens scripts in; `nano` by default. Arguments are allowed |
| Resolution | Screens | The mode Tessera asks each screen for: `preferred`, or a size like `1920x1080` or `1920x1080@60`. Applies at once |
| Qt theme plugin | General | `QT_QPA_PLATFORMTHEME` for programs Tessera starts. `kde` (the default) gives KDE apps Breeze and your KDE settings, and needs `plasma-integration`. Empty leaves Qt plain |
| GTK theme | General | `GTK_THEME` for programs Tessera starts, e.g. `Breeze-Dark`. Empty leaves GTK to its own settings |
| Terminal share when tiled beside (%) | Launcher | A terminal's share of the launcher's tile; the launcher opens at the rest |
| Launcher side | Launcher | Which side of the focused window the launcher opens on; terminals go on its other side |

---

## Driving Tessera from a script

Anything Tessera starts gets `TESSERA_SOCKET` in its environment, naming a socket that speaks one JSON object per line:

```bash
echo '{"type":"ListWindows"}' | socat - UNIX-CONNECT:$TESSERA_SOCKET
# {"type":"Windows","windows":[{"id":1,"app_id":"foot","title":"~","workspace":1,"focused":true}]}

# Open a program, tiled the usual way
echo '{"type":"Spawn","argv":["firefox"]}' | socat - UNIX-CONNECT:$TESSERA_SOCKET

# Open one beside *this* window, taking 60% of its space
echo '{"type":"Spawn","argv":["foot"],"placement":{"type":"beside_caller","side":"right","ratio":0.6}}' \
  | socat - UNIX-CONNECT:$TESSERA_SOCKET

# Focus a window from the list
echo '{"type":"Focus","window":1}' | socat - UNIX-CONNECT:$TESSERA_SOCKET

# Re-read config.toml after editing it by hand
echo '{"type":"ReloadConfig"}' | socat - UNIX-CONNECT:$TESSERA_SOCKET
# {"type":"ConfigApplied","live":["layout.gaps"],"needs_restart":[]}

# Scripts: list, run (optionally "mode":"terminal"), stop, read output
echo '{"type":"ListScripts"}' | socat - UNIX-CONNECT:$TESSERA_SOCKET
echo '{"type":"RunScript","name":"sync-notes"}' | socat - UNIX-CONNECT:$TESSERA_SOCKET
echo '{"type":"StopScript","name":"sync-notes"}' | socat - UNIX-CONNECT:$TESSERA_SOCKET
echo '{"type":"ScriptOutput","name":"sync-notes","tail":20}' | socat - UNIX-CONNECT:$TESSERA_SOCKET

# Watch events: ScriptStarted, ScriptExited, ScriptsChanged, one per line
(echo '{"type":"Subscribe"}'; cat) | socat - UNIX-CONNECT:$TESSERA_SOCKET

# End the session (what Power & session → Log out does)
echo '{"type":"Quit"}' | socat - UNIX-CONNECT:$TESSERA_SOCKET
```

Programs Tessera starts also get `TESSERA_BACKEND`, `nested` or `session`.

Scripts are named by file name, not by their header's `name`.

`beside_caller` works from any process running inside a Tessera window, including a shell script in a terminal: the compositor works out which window you are in from the connection itself. `side` is `left`, `right`, `above` or `below`; `ratio` is the new window's share, 0.1–0.9.

---

## Troubleshooting

**The launcher doesn't open.** While nested, the host desktop may take `Mod+Space` first (Plasma opens KRunner) — use **`Mod+\`** instead. If neither works, check the log for `could not start the launcher`: the binary is looked for next to `tessera-comp` and then on `PATH`.

**Everything freezes while the screen is locked.** Fixed — clients are flushed from the event loop rather than the redraw path. If something similar reappears, check `RUST_LOG=debug` for `failed to flush clients`.

**GL apps print `libEGL warning: failed to get driver name`.** Nested Tessera does not advertise `linux-dmabuf`, so GL clients fall back to software rendering. They work, just more slowly. As your session, dmabuf is advertised and they render on the GPU.

**Tessera as a session shows nothing, or exits at once.** Read
`~/.local/state/tessera/tessera.log`. "could not join a seat session" means no
seatd or logind; "no GPU found" means udev offered no DRM device; "no screen is
connected" means the connectors came back empty, in which case
`TESSERA_DRM_DEVICE` may be pointing at the wrong card.

**Stuck with a black screen.** `Ctrl+Alt+F1`…`F12` switches to another virtual
terminal, from where you can log in and stop it.

**After a Tessera session, another desktop behaves oddly** — KDE's display
settings report "no kscreen backend", hotplugging a monitor does nothing, or
portals fail. One `systemd --user` is shared by all of your sessions, and
Tessera puts `WAYLAND_DISPLAY` there so portals can find it. It restores the
old value when it exits, but a crash leaves Tessera's behind. From a terminal
in the other desktop:

```bash
systemctl --user import-environment WAYLAND_DISPLAY
```

Logging out and back in does the same thing.

**Blurry text on a HiDPI screen.** Only integer scaling is supported; fractional scaling comes later.

**Everything crawls on a 4K screen.** Tessera draws the whole screen every
frame, and on an integrated GPU 4K is four times the work of 1080p. Set
Configuration → Screens → *Resolution* to `1920x1080`; it applies at once, with
no need to unplug anything.

**A window opened in the wrong place.** Placement matching waits up to 3 seconds for the window and falls back to "the oldest waiting spawn" for programs it cannot identify (single-instance terminals such as `footclient`). Starting two such programs at once can swap their placements.

**X11-only applications don't start.** There is no Xwayland support, and it is out of scope for v0.1. Applications that *can* speak Wayland but guess wrongly are handled: Tessera sets the toolkit variables above for everything it starts.

**KDE apps ignore your Breeze theme.** Qt only loads the KDE theme plugin when told to. Configuration → General → *Qt theme plugin* does that, and is `kde` by default; it needs `plasma-integration` installed. GTK apps follow *GTK theme* in the same menu, or their own settings when it is empty.

**`E` does nothing, or says "No editor found".** The editor in Configuration → General is not installed, and neither `$VISUAL`, `$EDITOR` nor any common editor was found. Set the Editor setting to one you have.

**A script's key does nothing.** Select it in Scripts & services: a `(!)` and a red line explain a clashing or unreadable binding. Bindings need `Mod`, like every other.

**Starting a service opens a terminal.** systemd needs your password for that action; type it there. Services started with `--user` never ask.

**Nothing appears when running the launcher in a terminal inside a script.** ratatui draws nothing in a zero-size pty; set a size (`stty rows 24 cols 80`) if you are driving it from a test harness.

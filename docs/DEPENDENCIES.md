# Tessera — Dependencies & Tools

Everything needed to build, run and test Tessera, grouped by what it's for and the stage of development that first needed it.

**Keep this file current.** When a stage adds a crate, system library or tool, add it here in the same change, and move it from *planned* to *in use*.

Status key: **verified** = installed and exercised in S0 · **expected** = required by the design, confirm when its stage starts.

---

## 1. Quick install

### Arch Linux
```bash
# S0–S6 (development, nested)
sudo pacman -S --needed base-devel pkgconf git rustup \
  wayland wayland-protocols libxkbcommon libinput mesa libglvnd seatd libdrm systemd-libs \
  foot alacritty socat nano polkit ttf-jetbrains-mono
rustup default stable

# Optional debugging helpers
sudo pacman -S --needed wayland-utils libinput-tools gtk4-demos

# S7 (real session)
sudo pacman -S --needed greetd drm_info     # or use an existing GDM/SDDM
```

### Debian / Ubuntu
```bash
sudo apt install build-essential pkg-config git curl \
  libwayland-dev wayland-protocols libxkbcommon-dev libinput-dev libudev-dev \
  libgbm-dev libseat-dev libegl-dev libgles-dev libdrm-dev \
  foot alacritty socat fonts-jetbrains-mono
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh    # then: rustup default stable
```

### Fedora
```bash
sudo dnf install gcc pkgconf-pkg-config git rustup \
  wayland-devel wayland-protocols-devel libxkbcommon-devel libinput-devel systemd-devel \
  mesa-libgbm-devel libseat-devel mesa-libEGL-devel mesa-libGLES-devel libdrm-devel \
  foot alacritty socat jetbrains-mono-fonts
rustup-init    # then: rustup default stable
```

> The Arch list is what was installed and verified on 2026-09-15, first on WSL2 and then on the native Arch + KDE Plasma laptop. The Debian and Fedora names are equivalents that haven't been tested; fix them here if a package name turns out wrong.

---

## 2. Toolchain

| Tool | Version | Needed for | Status |
|------|---------|-----------|--------|
| rustup | any | Manages the toolchain in `rust-toolchain.toml` | verified |
| rustc / cargo | stable (1.98.1 at S0) | Everything; workspace uses **edition 2024** (needs ≥ 1.85) | verified |
| clippy, rustfmt | from stable | S0 onward; `cargo clippy --workspace -- -D warnings` is a Verify step | verified |
| C compiler (gcc or clang) | any | Linker, and C code in some `-sys` crates | verified (gcc 16.1.1 on the laptop) |
| pkg-config / pkgconf | any | Finds system libraries for `-sys` crates (libinput, libudev, libseat, libgbm) | verified |
| git | any | Repo; Smithay reference checkout | verified |

---

## 3. System libraries

**Link time** means you need the development package (headers + `.pc` file) to compile. **Runtime** means the library is loaded with `dlopen` when the program runs, so only the regular package is needed.

| Library | Arch package | Used by | How | First stage | Status |
|---------|--------------|---------|-----|-------------|--------|
| libxkbcommon | `libxkbcommon` | smithay keyboard handling (`xkbcommon` crate); SCTK keymaps in the launcher | link (comp), runtime (launcher) | S1 | verified (smallvil built) |
| xkeyboard-config | pulled in by libxkbcommon | Keymap data (`us` layout etc.) | runtime data | S1 | verified |
| libwayland-client | `wayland` | winit backend connecting to the host desktop (`wayland-dlopen`) | runtime | S1 | verified |
| wayland-protocols | `wayland-protocols` | Protocol XML reference; handy with `wayland-info` | reference only | S1 | installed |
| libEGL, libGLESv2 | `mesa`, `libglvnd` | smithay `renderer_gl` / EGL, nested and real | runtime | S1 | verified (Intel UHD 620 hardware GL ES 3.2) |
| libX11, libXcursor, libxkbcommon-x11 | pulled in by mesa/alacritty | winit's X11 fallback if the host session is X11 | runtime | S1 | installed |
| libudev | `systemd-libs` | smithay `backend_udev`: GPU/input device discovery | link | S7 | installed |
| libinput | `libinput` | smithay `backend_libinput`: keyboards, mice, touchpads on a TTY | link | S7 | installed |
| libseat + seatd | `seatd` | smithay `backend_session_libseat`: device access without root, VT switching | link + service | S7 | installed (seatd disabled; systemd-logind active, which libseat can use instead) |
| libgbm | `mesa` | smithay `backend_gbm`: GPU buffer allocation for DRM output | link | S7 | installed |
| libdrm | `libdrm` | Pulled in by Mesa; `drm_info` debugging. The `drm` crate talks to the kernel directly | indirect | S7 | installed |
| A Vulkan loader | `vulkan-icd-loader` | **Not needed.** Only removes a harmless Mesa zink warning | — | — | not installed |

### Services & kernel requirements (S7 only)
- **seatd** (`systemctl enable --now seatd` and add your user to the `seat` group) **or** systemd-logind. libseat uses whichever is available.
- **polkit** for service passwords (S6), and **systemd** for `systemctl --user import-environment`, which is how portals learn `WAYLAND_DISPLAY`.
- A kernel with DRM/KMS for your GPU and **pidfd** support (Linux ≥ 5.3; used by the S6 supervisor).
- A normal user account (the laptop runs as `Nathan`; the old WSL setup ran as root).

---

## 4. Rust crates

Versions marked "latest at S0" were current on crates.io on 2026-09-15. Pin the actual version when a crate is added.

### In use now (workspace `Cargo.toml`)

| Crate | Version | Used by | Purpose |
|-------|---------|---------|---------|
| `smithay` | `=0.7.0`, `default-features = false` | tessera-comp | Compositor framework; features added per stage below |
| `calloop` | `0.14` | (declared) | Event loop; tessera-comp uses smithay's re-export (`smithay::reexports::calloop`) so versions can't drift |
| `tracing` | `0.1` | both binaries | Logging |
| `tracing-subscriber` | `0.3`, feature `env-filter` | both binaries | Log output; filter with e.g. `RUST_LOG=debug` |
| `anyhow` | `1` | both binaries | Error handling in `main` |
| `thiserror` | `2` | cells, config | Typed errors |
| `ratatui` | `0.30`, no default features | cells, launcher | Widgets; `CellBackend` implements its `Backend` trait |
| `crossterm` | `0.29` | launcher | `--tty` front end (via ratatui's `crossterm_0_29` feature) |
| `smithay-client-toolkit` | `0.21` | launcher | Wayland window, shm pool, keyboard repeat, pointer |
| `fontdb` | `0.24` | cells | Finds system fonts by family |
| `swash` | `0.2` | cells | Glyph rasterisation and metrics |
| `freedesktop-desktop-entry` | `0.8` | launcher | Reads `.desktop` files |
| `nucleo-matcher` | `0.3` | launcher | Fuzzy app filtering |
| `image` (dev) | `0.25` | cells | Reserved for image-based tests |
| `serde` (+ derive) | `1` | ipc | Message types |
| `serde_json` | `1` | ipc, comp, launcher | JSON-lines framing; the launcher parses the event stream line by line |
| `rustix` | `1`, features `net` (workspace), `fs` + `process` (comp) | comp | `SO_PEERCRED`; `pidfd_open` and process-group signals for the S6 supervisor; inotify for the scripts folder; `O_NONBLOCK` on pipes |
| `toml_edit` | `0.25`, feature `serde` | config, services | Edits config.toml and services.toml keeping comments; parses the schema and script headers |
| `tessera-cells` | path | comp | Measures the launcher font so cell snapping matches exactly |
| `tessera-services` | path | launcher | The tracked-services list and the `systemctl` wrapper; also builds the `tessera-serv` binary |

### smithay features by stage

| Stage | Features to enable in `tessera-comp` |
|-------|---------------------------------------|
| S1 (**in use**) | `backend_winit`, `renderer_gl`, `desktop`, `wayland_frontend` |
| S7 (**in use**) | + `backend_udev`, `backend_drm`, `backend_gbm`, `backend_libinput`, `backend_session_libseat`, `backend_egl` |
| not planned | `xwayland` (X11 apps are out of scope for v0.1) |

### Considered and not used

| Crate | Why not |
|-------|---------|
| `notify` | Planned for watching the scripts folder in S6. rustix's inotify does the job as one more fd in the calloop loop, with no watcher thread, and `notify`'s newest line (9.x) was still a release candidate. See PLAN, Decisions log, 2026-09-18 |
| `zbus` | Would let Tessera talk to systemd and polkit over D-Bus directly. Running `systemctl` is simpler and its output is what people already know; revisit for a Tessera-styled polkit agent |
| `smithay-drm-extras` | anvil uses it to scan connectors and read EDID. It pulls in `libdisplay-info`, which does not build at the pinned Smithay tag on current Arch (NOTES, S2), so S7 scans connectors itself and names screens by connector |

---

## 5. Runtime & test tools

| Tool | Arch package | Used in | Why | Status |
|------|--------------|---------|-----|--------|
| foot | `foot` | S1 onward | Main test client; default terminal command | verified (ran in nested smallvil, confirmed by screenshot) |
| alacritty | `alacritty` | S4 | Second terminal for window-matching tests | installed |
| socat | `socat` | S4 | Talk to the IPC socket by hand | installed |
| JetBrains Mono | `ttf-jetbrains-mono` | S3 | Default launcher font; golden tests need a fixed font | installed |
| A GTK4 app, e.g. `gtk4-demo` | `gtk4-demos` | S2 | Check server-side decorations remove client title bars | optional |
| wayland-info | `wayland-utils` | any | List globals/protocols the compositor advertises | optional |
| libinput debug-events | `libinput-tools` | S7 | Check input devices from a TTY | optional |
| drm_info | `drm_info` | S7 | Inspect connectors, modes and planes | optional |
| greetd (or GDM/SDDM) | `greetd` | S7 | Log in to the Tessera session | S7 |
| nano | `nano` | S6 | Default editor for "Edit script" (Configuration → General → Editor); `$VISUAL`, `$EDITOR` and other common editors are tried if it is missing | installed |
| systemctl | `systemd` | S6 | Services in Scripts & services, and `tessera-serv` | installed |
| pkttyagent | `polkit` | S6 | Asks for the password when a system service action needs one; systemctl starts it itself in a terminal | installed |

---

## 6. Reference sources (not dependencies)

| What | Where | Used for |
|------|-------|----------|
| Smithay source at tag `v0.7.0` | `git clone https://github.com/Smithay/smithay ~/src/smithay && git -C ~/src/smithay checkout v0.7.0` | `smallvil` (S1 structure), `anvil` (S2 hardware smoke test, S7 udev backend) |
| smithay docs | https://docs.rs/smithay/0.7.0 | API for the pinned version |
| SCTK docs | https://docs.rs/smithay-client-toolkit | Launcher window/input |
| ratatui docs | https://docs.rs/ratatui | `Backend` trait for the pinned version |
| Wayland protocol explorer | https://wayland.app/protocols | xdg-shell, xdg-activation, xdg-decoration, layer-shell details |

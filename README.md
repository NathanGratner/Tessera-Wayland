# Tessera

(This repo only has 2 commits because I committed a handoff doc meant for claude that had info specific to my laptop so I just made a new repo) 

A small tiling Wayland compositor with a keyboard-driven launcher drawn on a character grid, in the style of the Linux kernel's `make menuconfig`.

Tessera is two programs that work as a pair:

- **`tessera-comp`** — the compositor. It tiles windows in a binary split tree, has nine workspaces, and is driven from the keyboard.
- **`tessera-launcher`** — the launcher. It looks like a terminal program but paints its own pixels, so it gets real mouse input, crisp box-drawing lines and exact sizing. It runs as a tiled window inside Tessera, or in any terminal with `--tty`.

Built on [Smithay](https://github.com/Smithay/smithay). Rust, AGPL-3.0-or-later.

Install it on Arch with the recipe in [`packaging/arch`](packaging/arch):
`makepkg -si` builds it, runs the tests and adds **Tessera** to your display
manager's session list.

> **Status: in development.** The compositor tiles, the launcher opens programs and terminals beside itself, settings are edited in a menuconfig-style screen and apply live, and scripts and systemd services are managed from one screen. It can run as your session on real hardware, with the limits listed in [`docs/USAGE.md`](docs/USAGE.md).

## Try it

Needs Linux, Rust stable and a few system libraries — the full list per distro is in [`docs/DEPENDENCIES.md`](docs/DEPENDENCIES.md).

```bash
# Arch
sudo pacman -S --needed base-devel pkgconf wayland wayland-protocols libxkbcommon \
  libinput mesa libglvnd seatd libdrm foot ttf-jetbrains-mono

cargo build
cargo run -p tessera-comp -- --nested --spawn foot
```

That opens Tessera as a window inside your current desktop, with a terminal tiled inside it. **Alt+\\** opens the launcher (Alt+Space also works, where the host desktop doesn't take it first), **Alt+Return** another terminal, and **Alt+Shift+E** quits.

To run it as your actual session instead, install it and log in on a TTY or
pick **Tessera** in your display manager; see [`docs/USAGE.md`](docs/USAGE.md).
There, Mod is **Super** rather than Alt.

The launcher also runs on its own, in any terminal:

```bash
cargo run -p tessera-launcher -- --tty
```

Scripts go in `~/.config/tessera/scripts` (any executable file), and systemd services are added with `tessera-serv`, for example `tessera-serv enable sshd`. Both appear under **Scripts & services** in the launcher. See [`docs/USAGE.md`](docs/USAGE.md).

## Documentation

| Document | What it covers |
|----------|----------------|
| [`docs/USAGE.md`](docs/USAGE.md) | Running Tessera, every key binding, launcher navigation, troubleshooting |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | How the pieces fit together and where each responsibility lives |
| [`docs/DEPENDENCIES.md`](docs/DEPENDENCIES.md) | Every system package, crate and tool, by stage |

API documentation comes from rustdoc:

```bash
cargo doc --workspace --no-deps --open
```

## Layout

```
crates/
  tessera-comp/      the compositor (binary)
  tessera-launcher/  the launcher (binary)
  tessera-cells/     character grid, glyph atlas and CPU painter (library)
  tessera-ipc/       compositor/launcher messages (library)
  tessera-config/    settings schema, values and script headers (library)
  tessera-services/  tracked systemd services (library) and tessera-serv (binary)
```

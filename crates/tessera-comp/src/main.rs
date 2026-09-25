//! Tessera compositor. Built up stage by stage; see docs/PLAN.md.

#![warn(missing_docs)]

mod backend;
mod handlers;
mod input;
mod ipc;
mod layout;
mod scripts;
mod settings;
mod state;

use std::io::IsTerminal;

use anyhow::{Context, bail};
use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

pub use state::{ModKey, Tessera};

const USAGE: &str = "\
Usage: tessera-comp [--backend auto|winit|udev] [--spawn <command>]...

  --backend <name>    How to show the screen:
                        auto   a window if a desktop session is running, else the hardware (default)
                        winit  a window inside the current desktop
                        udev   the hardware directly, from a TTY: Tessera is the session
  --nested            The same as --backend winit
  --spawn <command>   Run a shell command connected to this compositor once it starts (repeatable)
  -h, --help          Show this help

Logging is controlled with RUST_LOG, e.g. RUST_LOG=debug. When Tessera is the
session and nothing is attached to the terminal, the log goes to
$XDG_STATE_HOME/tessera/tessera.log.";

/// Overrides the refusal to take the screen from a running desktop session.
const FORCE_UDEV_ENV: &str = "TESSERA_FORCE_UDEV";

/// How the compositor talks to the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// A window inside another desktop.
    Winit,
    /// Real hardware through udev, DRM and libinput.
    Udev,
}

struct Args {
    backend: Backend,
    spawn: Vec<String>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut wanted: Option<Backend> = None;
    let mut spawn = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--nested" => wanted = Some(Backend::Winit),
            "--backend" => {
                let name = args.next().context("--backend needs a name")?;
                wanted = match name.as_str() {
                    "auto" => None,
                    "winit" | "nested" => Some(Backend::Winit),
                    "udev" | "tty" => Some(Backend::Udev),
                    other => bail!("unknown backend `{other}`\n\n{USAGE}"),
                };
            }
            "--spawn" => spawn.push(args.next().context("--spawn needs a command")?),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("unknown argument `{other}`\n\n{USAGE}"),
        }
    }

    // A desktop session around us means a window; a bare TTY means the hardware.
    let backend = wanted.unwrap_or(if backend::udev::looks_like_a_tty() {
        Backend::Udev
    } else {
        Backend::Winit
    });

    if backend == Backend::Winit && backend::udev::looks_like_a_tty() {
        bail!(
            "no desktop session to open a window in. From a TTY, run without --nested to use the \
             hardware directly."
        );
    }

    // Two compositors cannot both drive the screen: the udev backend takes
    // DRM master, which the running desktop already holds. Asking for it from
    // inside a session is nearly always a mistake, and an expensive one.
    if backend == Backend::Udev
        && !backend::udev::looks_like_a_tty()
        && std::env::var_os(FORCE_UDEV_ENV).is_none()
    {
        bail!(
            "refusing to take the screen while a desktop session is running: it would fight the \
             session for the display and could leave you with a black screen.\n\nSwitch to a TTY \
             (Ctrl+Alt+F3), log in there, and run it again. Set {FORCE_UDEV_ENV}=1 to override."
        );
    }

    Ok(Args { backend, spawn })
}

/// Sets up logging. As the session there is usually no terminal to print to,
/// so the log goes to a file instead — and a compositor with nowhere to report
/// its errors is a compositor nobody can fix.
fn init_logging(backend: Backend) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let to_file = backend == Backend::Udev && !std::io::stderr().is_terminal();

    if to_file {
        let path = backend::udev::log_path();
        let opened = backend::udev::open_log(&path);
        match opened {
            Ok(file) => {
                tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_ansi(false)
                    .with_writer(std::sync::Mutex::new(file))
                    .init();
                return;
            }
            Err(err) => {
                eprintln!("tessera: cannot write {}: {err}", path.display());
            }
        }
    }
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    init_logging(args.backend);

    let mut event_loop: EventLoop<'static, Tessera> =
        EventLoop::try_new().context("failed to create the event loop")?;
    let display: Display<Tessera> =
        Display::new().context("failed to create the Wayland display")?;

    let nested = args.backend == Backend::Winit;
    let mut state = Tessera::new(&mut event_loop, display, ModKey::for_backend(nested))?;
    state.nested = nested;
    // Before the backend: the udev backend asks the configuration which colour
    // formats and scan-out behaviour to set a screen up with.
    state.load_initial_config();
    match args.backend {
        Backend::Winit => backend::winit::init(&mut event_loop, &mut state)?,
        Backend::Udev => backend::udev::init(&mut event_loop, &mut state)?,
    }

    // The launcher and any child process find this through TESSERA_SOCKET.
    let socket_path = tessera_ipc::socket_path(&state.socket_name.to_string_lossy());
    match ipc::IpcServer::bind(&event_loop.handle(), &socket_path) {
        Ok(server) => {
            tracing::info!(path = %server.path().display(), "ipc socket ready");
            state.ipc = Some(server);
        }
        // Without it the compositor still works; only placement requests fail.
        Err(err) => tracing::warn!(error = %format!("{err:#}"), "no ipc socket"),
    }

    tracing::info!(
        socket = %state.socket_name.to_string_lossy(),
        mod_key = ?state.mod_key,
        backend = ?args.backend,
        "tessera-comp running; connect clients with WAYLAND_DISPLAY={}, quit with {:?}+Shift+E",
        state.socket_name.to_string_lossy(),
        state.mod_key,
    );

    // After the socket, so scripts inherit TESSERA_SOCKET.
    state.init_scripts();

    // Shared with every other session this user has open, so it is borrowed
    // rather than taken: see `export_wayland_display`.
    let previous_display = (args.backend == Backend::Udev)
        .then(|| backend::udev::export_wayland_display(&state.socket_name))
        .flatten();

    for command in &args.spawn {
        state.spawn(command);
    }

    // Housekeeping after every dispatch, not per frame: clients must keep making progress
    // even when the host stops sending frame callbacks (e.g. while the screen is locked).
    event_loop
        .run(None, &mut state, |state| {
            state.space.refresh();
            state.popups.cleanup();
            if let Err(err) = state.display_handle.flush_clients() {
                tracing::warn!(%err, "failed to flush clients");
            }
        })
        .context("event loop failed")?;

    // Scripts belong to the session; they end with it.
    state.stop_all_scripts();
    if args.backend == Backend::Udev {
        backend::udev::restore_wayland_display(previous_display);
    }
    tracing::info!("tessera-comp exited cleanly");
    Ok(())
}

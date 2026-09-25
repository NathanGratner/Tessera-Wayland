//! Tessera launcher: a menuconfig-style menu, in a terminal or as its own window.

#![warn(missing_docs)]

mod app;
mod apps;
mod config_menu;
mod event;
mod frontend;
mod ipc;
mod power_menu;
mod scripts_menu;
mod ui;
mod worker;

use anyhow::{Context, bail};

use tessera_config::{ConfigValues, config_path};

use crate::{app::App, apps::Catalog};

const USAGE: &str = "\
Usage: tessera-launcher [--apps] [--tty] [--font <family>] [--font-size <px>]

  --apps              Only the application list, as an overlay centred over
                      the windows; launching or pressing Esc closes it
  --tty               Run in the current terminal instead of opening a window
  --font <family>     Font for the window front end (default: from config.toml)
  --font-size <px>    Font size in pixels (default: from config.toml)
  -h, --help          Show this help

With no options the launcher opens a window when a Wayland compositor is
available, and falls back to the terminal otherwise.";

struct Args {
    tty: bool,
    apps: bool,
    /// Command-line overrides; config.toml decides otherwise.
    font: Option<String>,
    font_size: Option<f32>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args {
        tty: false,
        apps: false,
        font: None,
        font_size: None,
    };

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--tty" => args.tty = true,
            "--apps" => args.apps = true,
            "--font" => args.font = Some(iter.next().context("--font needs a family name")?),
            "--font-size" => {
                args.font_size = Some(
                    iter.next()
                        .context("--font-size needs a number")?
                        .parse()
                        .context("--font-size must be a number of pixels")?,
                );
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("unknown argument `{other}`\n\n{USAGE}"),
        }
    }
    Ok(args)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = parse_args()?;

    // A broken config.toml should not stop the launcher; it opens with
    // defaults and says what is wrong when the configuration menu is opened.
    let (config, problem) = match ConfigValues::load(&config_path()) {
        Ok(values) => (values, None),
        Err(err) => (ConfigValues::default(), Some(err.to_string())),
    };
    let font = args.font.unwrap_or_else(|| config.text("launcher.font"));
    let font_size = args
        .font_size
        .unwrap_or(config.int("launcher.font_size") as f32);

    let mut app = if args.apps {
        App::apps_only(Catalog::load(), config)
    } else {
        App::new(Catalog::load(), config)
    };
    if let Some(problem) = problem {
        tracing::warn!(problem, "config.toml is unusable; using defaults");
        app.config_problem(format!("config.toml ignored: {problem}"));
    }

    let wayland = !args.tty && std::env::var_os("WAYLAND_DISPLAY").is_some();
    if wayland {
        return frontend::wayland::run(app, &font, font_size, args.apps)
            .map_err(|err| anyhow::anyhow!("{err:#}"));
    }
    frontend::tty::run(app)
}

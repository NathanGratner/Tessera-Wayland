//! `tessera-serv`: run a systemctl command and keep the unit in Tessera's list.
//!
//! `tessera-serv enable sshd` is `systemctl enable sshd.service`, after which
//! sshd appears in the launcher's Scripts & services screen. systemctl runs
//! attached to this terminal, so if it needs a password it asks here.

use std::{
    io::{self, BufRead, Write},
    process::ExitCode,
};

use tessera_services::{
    Scope, Service, ServiceList, Verb, services_path,
    systemctl::{self, systemctl},
};

const USAGE: &str = "\
Usage: tessera-serv <command> [--user] [--now] [--pause] <unit>...

Commands:
  enable, start, restart   run systemctl, then show the unit in Tessera
  disable, stop            run systemctl (the unit stays listed)
  status                   systemctl status
  add, remove              only change which units Tessera shows
  list                     the units Tessera shows, and their state

Options:
  --user    a user service (systemctl --user); no password needed
  --now     with enable or disable: also start or stop it
  --pause   if anything failed, wait for Enter before exiting
            (the launcher uses this for the terminal it opens)

Units without a type get .service, as with systemctl. The list lives in
~/.config/tessera/services.toml.";

struct Args {
    command: String,
    units: Vec<String>,
    scope: Scope,
    now: bool,
    pause: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut command = None;
    let mut units = Vec::new();
    let mut scope = Scope::System;
    let mut now = false;
    let mut pause = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--user" => scope = Scope::User,
            "--system" => scope = Scope::System,
            "--now" => now = true,
            "--pause" => pause = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            flag if flag.starts_with('-') => return Err(format!("unknown option `{flag}`")),
            _ if command.is_none() => command = Some(arg),
            _ => units.push(arg),
        }
    }
    let command = command.ok_or("no command given")?;
    Ok(Args {
        command,
        units,
        scope,
        now,
        pause,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("tessera-serv: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let pause = args.pause;
    let code = match run(args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("tessera-serv: {message}");
            1
        }
    };
    if pause && code != 0 {
        print!("\nPress Enter to close.");
        let _ = io::stdout().flush();
        let _ = io::stdin().lock().read_line(&mut String::new());
    }
    ExitCode::from(code)
}

fn run(args: Args) -> Result<u8, String> {
    if args.command == "list" {
        return list();
    }
    if args.units.is_empty() {
        return Err(format!("`{}` needs a unit name", args.command));
    }
    let services: Vec<Service> = args
        .units
        .iter()
        .map(|unit| Service::new(unit, args.scope))
        .collect::<Result<_, _>>()?;

    match args.command.as_str() {
        "add" => {
            for service in &services {
                check_exists(service)?;
            }
            track(&services, true)?;
            Ok(0)
        }
        "remove" => {
            track(&services, false)?;
            Ok(0)
        }
        "status" => {
            let status = systemctl(args.scope)
                .args(["status", "--no-pager", "--"])
                .args(services.iter().map(|service| &service.unit))
                .status()
                .map_err(|err| format!("cannot run systemctl: {err}"))?;
            Ok(status.code().unwrap_or(1) as u8)
        }
        name => {
            let verb = Verb::parse(name).ok_or(format!("unknown command `{name}`"))?;
            let mut command = systemctl(args.scope);
            command.arg(verb.as_str());
            if args.now && matches!(verb, Verb::Enable | Verb::Disable) {
                command.arg("--now");
            }
            // Attached to this terminal, so systemctl can ask for a password here.
            let status = command
                .arg("--")
                .args(services.iter().map(|service| &service.unit))
                .status()
                .map_err(|err| format!("cannot run systemctl: {err}"))?;
            if !status.success() {
                return Ok(status.code().unwrap_or(1) as u8);
            }
            if matches!(verb, Verb::Enable | Verb::Start | Verb::Restart) {
                track(&services, true)?;
            }
            Ok(0)
        }
    }
}

/// Refuses units systemd has never heard of, so typos do not end up listed.
fn check_exists(service: &Service) -> Result<(), String> {
    let states = systemctl::query(std::slice::from_ref(service));
    match states.into_iter().next().flatten() {
        Some(state) if !state.exists() => Err(format!("systemd has no unit called {service}")),
        _ => Ok(()),
    }
}

/// Adds services to the list, or removes them, and says what changed.
fn track(services: &[Service], add: bool) -> Result<(), String> {
    let path = services_path();
    let mut list = ServiceList::load(&path)?;
    let mut changed = false;
    for service in services {
        let did = if add {
            list.add(service)
        } else {
            list.remove(service)
        };
        match (add, did) {
            (true, true) => println!("Tessera now shows {service} in Scripts & services."),
            (false, true) => println!("Tessera no longer shows {service}."),
            (false, false) => println!("{service} was not in Tessera's list."),
            (true, false) => {}
        }
        changed |= did;
    }
    if changed {
        list.save(&path)?;
    }
    Ok(())
}

fn list() -> Result<u8, String> {
    let services = ServiceList::load(&services_path())?.services();
    if services.is_empty() {
        println!("No services listed. Add one with `tessera-serv add <unit>`.");
        return Ok(0);
    }
    let states = systemctl::query(&services);
    for (service, state) in services.iter().zip(states) {
        let (active, enabled) = match &state {
            Some(state) if !state.exists() => ("not found".to_string(), String::new()),
            Some(state) => (
                format!("{} ({})", state.active, state.sub),
                state.enabled.clone(),
            ),
            None => ("unknown".to_string(), String::new()),
        };
        println!("{:<36} {:<22} {}", service.to_string(), active, enabled);
    }
    Ok(0)
}

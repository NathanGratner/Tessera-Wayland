//! Work that happens off the event loop, and how its results come back.
//!
//! Three things cannot run inline in the launcher's loop: the compositor's
//! event stream (a connection that never ends), `systemctl` (which blocks for
//! as long as a service takes to start, or a password prompt takes to answer)
//! and the one-second tick that keeps timers moving. Each runs on a thread and
//! sends an [`Update`] back; each front end turns those into calls to
//! [`crate::app::App::background`] on its own loop.

use std::{
    io::{BufRead, BufReader},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use tessera_ipc::{Event, Request};
use tessera_services::{
    ActionError, PowerAction, Service, ServiceList, Verb, services_path, systemctl,
};

use crate::{ipc::Ipc, scripts_menu::ServiceRow};

/// A result from background work.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// Something happened in the compositor.
    Compositor(Event),
    /// The tracked services and their current state.
    Services(Result<Vec<ServiceRow>, String>),
    /// A systemctl action finished.
    ServiceDone {
        /// The service acted on.
        service: Service,
        /// What was asked.
        verb: Verb,
        /// Whether it worked, and if not whether a password would help.
        result: Result<(), ActionError>,
    },
    /// `systemctl status` output for one service.
    ServiceStatus {
        /// The service asked about.
        service: Service,
        /// Its status lines, or why they could not be read.
        result: Result<Vec<String>, String>,
    },
    /// `services.toml` was edited; the text says how.
    ServiceListChanged(Result<String, String>),
    /// A suspend, reboot or power-off came back from logind.
    PowerDone {
        /// What was asked.
        action: PowerAction,
        /// Whether it worked, and if not whether a password would help.
        result: Result<(), ActionError>,
    },
    /// A second passed.
    Tick,
}

/// Sends updates to whichever loop the front end runs.
#[derive(Clone)]
pub struct Worker {
    send: Arc<dyn Fn(Update) + Send + Sync>,
}

impl Worker {
    /// Wraps the front end's way of waking its loop.
    pub fn new(send: impl Fn(Update) + Send + 'static) -> Self {
        // A Mutex makes any sender Sync, so the worker can be shared between threads.
        let send = Mutex::new(send);
        Self {
            send: Arc::new(move |update| {
                if let Ok(send) = send.lock() {
                    send(update);
                }
            }),
        }
    }

    /// Hands an update to the loop.
    pub fn send(&self, update: Update) {
        (self.send)(update);
    }

    /// Runs `job` on its own thread and delivers what it returns.
    pub fn spawn(&self, job: impl FnOnce() -> Update + Send + 'static) {
        let worker = self.clone();
        thread::spawn(move || worker.send(job()));
    }

    /// Sends [`Update::Tick`] once a second, for as long as the launcher runs.
    pub fn start_ticking(&self) {
        let worker = self.clone();
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(1));
                worker.send(Update::Tick);
            }
        });
    }

    /// Holds a connection to the compositor open and forwards its events.
    ///
    /// The first line back is the `Ok` for the subscription itself; anything
    /// that is not an event is skipped. If the compositor goes away the
    /// thread ends quietly: the launcher cannot outlive its compositor anyway.
    pub fn subscribe(&self, ipc: &Ipc) {
        let stream = match ipc.subscribe() {
            Ok(stream) => stream,
            Err(err) => {
                tracing::warn!(%err, "no event stream from the compositor; the scripts screen will not update live");
                return;
            }
        };
        let worker = self.clone();
        thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                if let Ok(event) = serde_json::from_str::<Event>(&line) {
                    worker.send(Update::Compositor(event));
                }
            }
            tracing::debug!("the compositor closed the event stream");
        });
    }

    /// Reads the tracked services and asks systemd about each.
    pub fn query_services(&self) {
        self.spawn(|| {
            let result = ServiceList::load(&services_path()).map(|list| {
                let services = list.services();
                let states = systemctl::query(&services);
                services
                    .into_iter()
                    .zip(states)
                    .map(|(service, state)| ServiceRow { service, state })
                    .collect()
            });
            Update::Services(result)
        });
    }

    /// Runs `systemctl <verb>` without prompting for a password.
    pub fn service_action(&self, service: Service, verb: Verb) {
        self.spawn(move || {
            let result = systemctl::act(verb, &service);
            Update::ServiceDone {
                service,
                verb,
                result,
            }
        });
    }

    /// Suspends, reboots or powers off, without prompting for a password.
    pub fn power(&self, action: PowerAction) {
        self.spawn(move || Update::PowerDone {
            action,
            result: systemctl::power(action),
        });
    }

    /// Fetches `systemctl status` for the status box.
    pub fn service_status(&self, service: Service) {
        self.spawn(move || {
            let result = systemctl::status(&service, 30);
            Update::ServiceStatus { service, result }
        });
    }

    /// Adds a service to `services.toml`, refusing units systemd does not know.
    pub fn add_service(&self, service: Service) {
        self.spawn(move || {
            let result = (|| {
                if let Some(Some(state)) = systemctl::query(std::slice::from_ref(&service)).pop()
                    && !state.exists()
                {
                    return Err(format!("systemd has no unit called {service}"));
                }
                let path = services_path();
                let mut list = ServiceList::load(&path)?;
                if !list.add(&service) {
                    return Ok(format!("{service} is already listed"));
                }
                list.save(&path)?;
                Ok(format!("added {service}"))
            })();
            Update::ServiceListChanged(result)
        });
    }

    /// Removes a service from `services.toml`. The service itself is untouched.
    pub fn remove_service(&self, service: Service) {
        self.spawn(move || {
            let result = (|| {
                let path = services_path();
                let mut list = ServiceList::load(&path)?;
                if list.remove(&service) {
                    list.save(&path)?;
                }
                Ok(format!(
                    "{service} is no longer listed (the service is unchanged)"
                ))
            })();
            Update::ServiceListChanged(result)
        });
    }
}

impl Ipc {
    /// Opens a connection that stays open for events.
    pub fn subscribe(&self) -> std::io::Result<std::os::unix::net::UnixStream> {
        let stream = std::os::unix::net::UnixStream::connect(self.path())?;
        let mut writer = &stream;
        tessera_ipc::write_message(&mut writer, &Request::Subscribe)?;
        Ok(stream)
    }
}

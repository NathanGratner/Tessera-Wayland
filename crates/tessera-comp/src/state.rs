use std::{collections::HashMap, ffi::OsString, process::Command, sync::Arc, time::Instant};

use anyhow::{Context, anyhow};
use smithay::{
    desktop::{LayerSurface, PopupManager, Space, Window, WindowSurfaceType, layer_map_for_output},
    input::{Seat, SeatState, keyboard::ModifiersState},
    output::Output,
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, generic::Generic,
        },
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::{
            wlr_layer::{Layer, WlrLayerShellState},
            xdg::{XdgShellState, decoration::XdgDecorationState},
        },
        shm::ShmState,
        socket::ListeningSocketSource,
        xdg_activation::XdgActivationState,
    },
};

use crate::{
    input::Bindings,
    ipc::{IpcServer, PendingPlacement},
    layout::{LayoutOptions, LayoutTree},
    scripts::Scripts,
    settings::Behaviour,
};

/// Workspaces are fixed at nine, one per number key (design §3.4).
pub const WORKSPACE_COUNT: usize = 9;

/// The modifier used by every compositor binding ("Mod" in the plan and design doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModKey {
    /// Used while nested, because host desktops keep Super for themselves.
    Alt,
    /// Used when Tessera owns the session.
    Super,
}

impl ModKey {
    /// Host desktops grab Super while we run nested inside them, so nested runs use Alt.
    pub fn for_backend(nested: bool) -> Self {
        if nested { ModKey::Alt } else { ModKey::Super }
    }

    /// Whether this modifier is currently down.
    pub fn is_held(self, mods: &ModifiersState) -> bool {
        match self {
            ModKey::Alt => mods.alt,
            ModKey::Super => mods.logo,
        }
    }
}

/// All compositor state (design §3.1). Fields for later stages are added in those stages.
pub struct Tessera {
    /// When the compositor started; frame callbacks are timed against it.
    pub start_time: Instant,
    /// Handle used to talk to clients.
    pub display_handle: DisplayHandle,
    /// Stops the event loop, which quits the compositor.
    pub loop_signal: LoopSignal,
    /// Name of our Wayland socket, e.g. `wayland-1`.
    pub socket_name: OsString,
    /// The modifier every binding uses.
    pub mod_key: ModKey,
    /// Running as a window inside another desktop.
    pub nested: bool,
    /// The configuration in effect.
    pub settings: tessera_config::ConfigValues,
    /// Settings consulted on every event.
    pub behaviour: Behaviour,

    /// Where mapped windows live, for rendering and hit-testing.
    pub space: Space<Window>,
    /// Popup tracking (menus, tooltips).
    pub popups: PopupManager,
    /// One tiling tree per workspace.
    pub workspaces: Vec<LayoutTree<Window>>,
    /// Index into [`Self::workspaces`] of the workspace on screen.
    pub active_workspace: usize,
    /// Gaps and cell snapping used when applying a layout.
    pub layout_opts: LayoutOptions,
    /// The focused window, if any: the one new windows tile beside and
    /// bindings act on. It holds the keyboard unless [`Self::layer_focus`] is set.
    pub focus: Option<Window>,
    /// A layer surface holding the keyboard instead of the focused window,
    /// such as the application overlay.
    ///
    /// Kept apart from [`Self::focus`] so that an overlay borrowing the
    /// keyboard does not make Tessera forget which window was focused: what is
    /// launched from it tiles beside that window, and the keyboard goes back to
    /// it when the overlay closes.
    pub layer_focus: Option<LayerSurface>,
    /// Key bindings, built from the configuration.
    pub bindings: Bindings,

    /// Handle for adding event sources, e.g. one per IPC connection.
    pub loop_handle: LoopHandle<'static, Self>,
    /// The private socket the launcher talks to (§4), once it is bound.
    pub ipc: Option<IpcServer>,
    /// Spawns whose windows have not appeared yet (§8).
    pub pending_placements: Vec<PendingPlacement>,
    /// Ids handed out to windows, for `ListWindows` and `Focus`.
    pub window_ids: Vec<(tessera_ipc::WindowId, Window)>,
    /// Next id to hand out.
    pub next_window_id: u64,
    /// Activation tokens clients have presented, by surface.
    pub presented_tokens: HashMap<WlSurface, String>,
    /// Set when the next mapped window should be moved to another workspace.
    pub pending_workspace: Option<usize>,
    /// Connections that asked for events, by connection id.
    pub subscribers: Vec<(u64, std::os::unix::net::UnixStream)>,
    /// Next IPC connection id.
    pub next_connection_id: u64,
    /// User scripts and their processes (§7).
    pub scripts: Scripts,
    /// The real-hardware backend, when Tessera owns the session (§3.2).
    pub udev: Option<Box<crate::backend::udev::Udev>>,
    /// libinput, so a VT switch can hand the devices back.
    pub libinput: Option<smithay::reexports::input::Libinput>,
    /// Where the pointer is. libinput reports movement, not position.
    pub pointer_location: Point<f64, Logical>,

    /// `wl_compositor` state.
    pub compositor_state: CompositorState,
    /// `xdg_shell` state: toplevels and popups.
    pub xdg_shell_state: XdgShellState,
    /// `wlr-layer-shell` state: overlays, launchers, bars.
    pub layer_shell_state: WlrLayerShellState,
    /// `xdg-decoration` state; Tessera always answers "server side".
    pub xdg_decoration_state: XdgDecorationState,
    /// `xdg-activation` state, used to match spawned programs to their windows.
    pub xdg_activation_state: XdgActivationState,
    /// `wl_shm` state for shared-memory buffers.
    pub shm_state: ShmState,
    /// `wl_output` and `xdg-output` state.
    pub output_manager_state: OutputManagerState,
    /// Seat bookkeeping for keyboard and pointer.
    pub seat_state: SeatState<Self>,
    /// Clipboard and drag-and-drop state.
    pub data_device_state: DataDeviceState,
    /// The one seat; the nested backend never has more.
    pub seat: Seat<Self>,
}

impl Tessera {
    /// Creates the compositor state and starts listening for clients.
    pub fn new(
        event_loop: &mut EventLoop<'static, Self>,
        display: Display<Self>,
        mod_key: ModKey,
    ) -> anyhow::Result<Self> {
        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let xdg_activation_state = XdgActivationState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);

        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "seat0");
        // The nested backend always has exactly one keyboard and one pointer.
        seat.add_keyboard(Default::default(), 200, 25)
            .context("failed to set up the keyboard (is xkeyboard-config installed?)")?;
        seat.add_pointer();

        let socket_name = Self::init_wayland_listener(display, event_loop)?;

        Ok(Self {
            start_time: Instant::now(),
            display_handle: dh,
            loop_signal: event_loop.get_signal(),
            socket_name,
            mod_key,
            nested: false,
            settings: tessera_config::ConfigValues::default(),
            behaviour: Behaviour::default(),

            space: Space::default(),
            popups: PopupManager::default(),
            workspaces: (0..WORKSPACE_COUNT).map(|_| LayoutTree::new()).collect(),
            active_workspace: 0,
            // Replaced by `load_initial_config` before any client connects.
            layout_opts: LayoutOptions {
                gaps: 8,
                snap: None,
            },
            focus: None,
            layer_focus: None,
            bindings: Bindings::default(),

            loop_handle: event_loop.handle(),
            ipc: None,
            pending_placements: Vec::new(),
            window_ids: Vec::new(),
            next_window_id: 1,
            presented_tokens: HashMap::new(),
            pending_workspace: None,
            subscribers: Vec::new(),
            next_connection_id: 1,
            scripts: Scripts::default(),
            udev: None,
            libinput: None,
            pointer_location: (0.0, 0.0).into(),

            compositor_state,
            xdg_shell_state,
            layer_shell_state,
            xdg_decoration_state,
            xdg_activation_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            seat,
        })
    }

    fn init_wayland_listener(
        display: Display<Self>,
        event_loop: &mut EventLoop<'static, Self>,
    ) -> anyhow::Result<OsString> {
        let listening_socket =
            ListeningSocketSource::new_auto().context("failed to create a Wayland socket")?;
        let socket_name = listening_socket.socket_name().to_os_string();

        let handle = event_loop.handle();
        handle
            .insert_source(listening_socket, |client_stream, _, state| {
                if let Err(err) = state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                {
                    tracing::warn!(%err, "failed to add a new client");
                }
            })
            .map_err(|err| anyhow!("failed to register the Wayland socket: {}", err.error))?;

        handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // SAFETY: the display is owned by this event source and never dropped while
                    // the source is registered.
                    if let Err(err) = unsafe { display.get_mut().dispatch_clients(state) } {
                        tracing::warn!(%err, "failed to dispatch client requests");
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|err| anyhow!("failed to register the Wayland display: {}", err.error))?;

        Ok(socket_name)
    }

    /// Runs a shell command as a client of this compositor, tiled the usual way.
    pub fn spawn(&mut self, command: &str) {
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), command.to_string()];
        if let Err(err) = self.spawn_placed(&argv, tessera_ipc::Placement::Auto, None) {
            tracing::warn!(error = %format!("{err:#}"), command, "failed to spawn");
        }
    }

    /// Starts a program with the environment a Tessera client needs.
    ///
    /// Returns the process id, which the placement bookkeeping matches against
    /// the window when it appears.
    pub(crate) fn spawn_process(
        &self,
        argv: &[String],
        token: Option<&str>,
    ) -> std::io::Result<u32> {
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .env("WAYLAND_DISPLAY", &self.socket_name)
            // Keep X11-capable apps from opening on the host desktop instead of in Tessera.
            .env_remove("DISPLAY")
            // Tessera has no X11 at all, and toolkits that guess wrongly simply
            // fail to start: Qt 5 defaults to X11 unless told otherwise, which
            // is why a Qt app that worked nested (inside a desktop that sets
            // these) would not start in a Tessera session.
            .env("XDG_SESSION_TYPE", "wayland")
            // Lets the launcher warn that power actions reach the whole
            // computer when Tessera is only a window inside another desktop.
            .env(
                "TESSERA_BACKEND",
                if self.nested { "nested" } else { "session" },
            )
            .env("QT_QPA_PLATFORM", "wayland")
            .env("GDK_BACKEND", "wayland")
            .env("SDL_VIDEODRIVER", "wayland")
            .env("CLUTTER_BACKEND", "wayland")
            .env("MOZ_ENABLE_WAYLAND", "1");

        // Toolkit themes: which plugin draws Qt widgets, and which GTK theme.
        // Empty means "leave it to the toolkit's own defaults".
        if !self.behaviour.qt_platform_theme.is_empty() {
            command.env("QT_QPA_PLATFORMTHEME", &self.behaviour.qt_platform_theme);
        }
        if !self.behaviour.gtk_theme.is_empty() {
            command.env("GTK_THEME", &self.behaviour.gtk_theme);
        }
        if let Some(ipc) = &self.ipc {
            command.env(tessera_ipc::SOCKET_ENV, ipc.path());
        }
        if let Some(token) = token {
            command.env("XDG_ACTIVATION_TOKEN", token);
        }

        let mut child = command.spawn()?;
        let pid = child.id();
        // Reap the child so it doesn't linger as a zombie. The S6 supervisor replaces this.
        std::thread::spawn(move || child.wait());
        Ok(pid)
    }

    /// The surface under this point, with its position, for pointer events.
    ///
    /// Layers stack around the windows: overlay and top above them, bottom
    /// and background below.
    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let in_layer = |layers: &[Layer]| {
            let (layer, location) = self.layer_under(layers, pos)?;
            layer
                .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                .map(|(surface, point)| (surface, (point + location).to_f64()))
        };
        in_layer(&[Layer::Overlay, Layer::Top])
            .or_else(|| {
                self.space
                    .element_under(pos)
                    .and_then(|(window, location)| {
                        window
                            .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                            .map(|(surface, point)| (surface, (point + location).to_f64()))
                    })
            })
            .or_else(|| in_layer(&[Layer::Bottom, Layer::Background]))
    }

    /// Lets clients on this screen draw their next frame: its windows and its
    /// layer surfaces.
    ///
    /// A surface that is never sent a frame callback draws once and then
    /// waits forever, so anything drawn on the screen has to be in here.
    pub(crate) fn send_frames(&self, output: &Output) {
        let time = self.start_time.elapsed();
        let throttle = Some(std::time::Duration::ZERO);
        for window in self.space.elements() {
            window.send_frame(output, time, throttle, |_, _| Some(output.clone()));
        }
        for layer in layer_map_for_output(output).layers() {
            layer.send_frame(output, time, throttle, |_, _| Some(output.clone()));
        }
    }

    /// Finds the mapped window owning this toplevel surface.
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.space
            .elements()
            .find(|window| window.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .cloned()
    }

    /// Gives keyboard focus to `window`, or clears focus when `None`.
    pub fn focus_window(&mut self, window: Option<&Window>) {
        self.focus = window.cloned();

        for w in self.space.elements() {
            w.set_activated(Some(w) == window);
        }
        let toplevels: Vec<_> = self
            .space
            .elements()
            .filter_map(|w| w.toplevel().cloned())
            .collect();
        for toplevel in toplevels {
            toplevel.send_pending_configure();
        }

        self.update_keyboard_focus();
    }

    /// Points the keyboard at the layer surface holding it, else at the
    /// focused window. Does nothing when it is already there.
    pub(crate) fn update_keyboard_focus(&mut self) {
        let surface = match &self.layer_focus {
            Some(layer) => Some(layer.wl_surface().clone()),
            None => self
                .focus
                .as_ref()
                .and_then(|w| w.toplevel())
                .map(|t| t.wl_surface().clone()),
        };
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        if keyboard.current_focus() != surface {
            tracing::debug!(
                to = if self.layer_focus.is_some() {
                    "layer"
                } else if surface.is_some() {
                    "window"
                } else {
                    "nothing"
                },
                "keyboard focus"
            );
            keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        }
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

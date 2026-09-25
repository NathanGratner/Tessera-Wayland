//! Wayland front end: the launcher as a tiled window, or the application
//! overlay as a layer surface, painting its own cells either way.
//!
//! Only the surface differs between the two. Drawing, input and frame
//! callbacks are shared, and both kinds of configure feed the same resize.

use crate::{
    app::App,
    event::{Event, Key, KeyCode, Mods, MouseKind},
    frontend::run_effects,
    ipc::Ipc,
    worker::{Update, Worker},
};
use anyhow::{Context, anyhow};
use ratatui::Terminal;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_dispatch2, delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{EventLoop, LoopHandle, LoopSignal, channel},
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use tessera_cells::{
    CellBackend, Font, Grid, Palette, Surface,
    font::CellMetrics,
    paint::{self},
};

pub const APP_ID: &str = "tessera.launcher";
/// The overlay's layer-shell namespace; the compositor finds it by this name
/// to close it when the binding is pressed again.
pub const OVERLAY_NAMESPACE: &str = "tessera.apps";
const DEFAULT_COLS: u32 = 80;
const DEFAULT_ROWS: u32 = 24;
/// The overlay's size in cells, before it is fitted to the screen.
const OVERLAY_COLS: u32 = 64;
const OVERLAY_ROWS: u32 = 22;

/// What the launcher draws into.
enum Shell {
    /// The full launcher: an `xdg_toplevel`, tiled like any window.
    Window(Window),
    /// The application overlay: a layer surface, centred above the windows.
    Overlay {
        layer: LayerSurface,
        /// The size last asked of the compositor, in pixels.
        requested: (u32, u32),
    },
}

impl Shell {
    fn wl_surface(&self) -> &wl_surface::WlSurface {
        match self {
            Shell::Window(window) => window.wl_surface(),
            Shell::Overlay { layer, .. } => layer.wl_surface(),
        }
    }
}

/// The overlay's size for a screen of `screen` pixels: its usual size, or
/// less on a screen too small for it, leaving a margin so it still reads as
/// something floating over the windows.
fn overlay_size(metrics: &CellMetrics, screen: Option<(u32, u32)>) -> (u32, u32) {
    let (cell_w, cell_h) = (metrics.width.max(1), metrics.height.max(1));
    let (mut cols, mut rows) = (OVERLAY_COLS, OVERLAY_ROWS);
    if let Some((width, height)) = screen {
        cols = cols.min((width / cell_w).saturating_sub(4)).max(20);
        rows = rows.min((height / cell_h).saturating_sub(2)).max(8);
    }
    (cols * cell_w, rows * cell_h)
}

/// Runs the launcher on Wayland: the full menu in a window, or with
/// `overlay`, the application list as a centred layer surface.
pub fn run(app: App, font_family: &str, font_size: f32, overlay: bool) -> anyhow::Result<()> {
    let font = Font::load(font_family, font_size)
        .with_context(|| format!("could not load the font `{font_family}`"))?;
    let metrics = font.metrics();

    let conn = Connection::connect_to_env().context("no Wayland compositor to connect to")?;
    let (globals, event_queue) =
        registry_queue_init(&conn).context("the compositor sent no globals")?;
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<Launcher> =
        EventLoop::try_new().context("failed to create the event loop")?;
    WaylandSource::new(conn.clone(), event_queue)
        .insert(event_loop.handle())
        .map_err(|err| anyhow!("failed to watch the Wayland connection: {err}"))?;

    let compositor =
        CompositorState::bind(&globals, &qh).map_err(|_| anyhow!("wl_compositor is missing"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|_| anyhow!("wl_shm is missing"))?;

    let surface = compositor.create_surface(&qh);
    let (shell, width, height) = if overlay {
        let layer_shell = LayerShell::bind(&globals, &qh).map_err(|_| {
            anyhow!(
                "this compositor has no layer shell (zwlr_layer_shell_v1), so the overlay \
                 cannot open; run tessera-launcher without --apps"
            )
        })?;
        let layer = layer_shell.create_layer_surface(
            &qh,
            surface,
            Layer::Overlay,
            Some(OVERLAY_NAMESPACE),
            None,
        );
        // No anchors: the compositor centres it. Exclusive: it has the
        // keyboard for as long as it is open, which is what lets you just type.
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        // The screen size is not known yet; it is fitted when the output
        // reports it (see `OutputHandler`).
        let (width, height) = overlay_size(&metrics, None);
        layer.set_size(width, height);
        layer.commit();
        let shell = Shell::Overlay {
            layer,
            requested: (width, height),
        };
        (shell, width, height)
    } else {
        let xdg_shell =
            XdgShell::bind(&globals, &qh).map_err(|_| anyhow!("xdg_shell is missing"))?;
        let window = xdg_shell.create_window(surface, WindowDecorations::RequestServer, &qh);
        window.set_title("Tessera launcher");
        window.set_app_id(APP_ID);
        window.set_min_size(Some((metrics.width * 20, metrics.height * 8)));
        window.commit();
        (
            Shell::Window(window),
            metrics.width * DEFAULT_COLS,
            metrics.height * DEFAULT_ROWS,
        )
    };
    let pool = SlotPool::new((width * height * 4) as usize, &shm)
        .context("failed to create the shared-memory pool")?;

    let palette = Palette::default();
    let backend = CellBackend::new(
        Grid::new(
            (width / metrics.width.max(1)) as u16,
            (height / metrics.height.max(1)) as u16,
            palette.foreground,
            palette.background,
        ),
        palette,
        metrics,
    );

    // Background work (compositor events, systemctl, the tick) reports back
    // through a channel the event loop watches, so it is handled like input.
    let (sender, updates) = channel::channel::<Update>();
    let worker = Worker::new(move |update| {
        let _ = sender.send(update);
    });
    event_loop
        .handle()
        .insert_source(updates, |event, _, launcher: &mut Launcher| {
            if let channel::Event::Msg(update) = event {
                launcher.background(update);
            }
        })
        .map_err(|err| anyhow!("failed to watch the background channel: {}", err.error))?;
    worker.start_ticking();
    let ipc = Ipc::from_env();
    if let Some(ipc) = &ipc {
        worker.subscribe(ipc);
    }

    let mut launcher = Launcher {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        shell,
        buffer: None,
        width,
        height,
        configured: false,
        needs_redraw: true,
        keyboard: None,
        pointer: None,
        modifiers: Mods::default(),
        font,
        metrics,
        terminal: Terminal::new(backend).context("failed to set up the cell renderer")?,
        app,
        qh: qh.clone(),
        frame_pending: false,
        ipc,
        worker,
        loop_handle: event_loop.handle(),
        loop_signal: event_loop.get_signal(),
    };

    let result = event_loop.run(None, &mut launcher, |launcher| {
        if launcher.app.should_quit() {
            launcher.loop_signal.stop();
        }
    });
    match result {
        Ok(()) => Ok(()),
        // The compositor going away is how the launcher normally ends when
        // Tessera quits: that is not a failure worth printing.
        Err(err) if is_disconnect(&err) => {
            tracing::info!("the compositor closed the connection");
            Ok(())
        }
        Err(err) => Err(anyhow!("event loop failed: {err}")),
    }
}

struct Launcher {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    shell: Shell,
    buffer: Option<Buffer>,
    width: u32,
    height: u32,
    configured: bool,
    needs_redraw: bool,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    modifiers: Mods,
    font: Font,
    metrics: CellMetrics,
    terminal: Terminal<CellBackend>,
    app: App,
    /// For requesting a draw outside a Wayland callback.
    qh: QueueHandle<Launcher>,
    /// True between asking for a frame callback and receiving it.
    frame_pending: bool,
    /// The compositor socket, when Tessera started us.
    ipc: Option<Ipc>,
    /// Runs slow work off the event loop.
    worker: Worker,
    loop_handle: LoopHandle<'static, Launcher>,
    loop_signal: LoopSignal,
}

impl Launcher {
    fn handle(&mut self, event: Event) {
        let effects = self.app.update(event);
        self.carry_out(effects);
    }

    /// A background result or tick arrived.
    fn background(&mut self, update: Update) {
        let tick = update == Update::Tick;
        let effects = self.app.background(update);
        // A tick changes nothing visible unless the screen shows timers.
        if tick && effects.is_empty() && !self.app.is_live() {
            return;
        }
        self.carry_out(effects);
    }

    fn carry_out(&mut self, effects: Vec<crate::app::Effect>) {
        run_effects(&effects, self.ipc.as_ref(), &self.worker).deliver(&mut self.app);
        self.needs_redraw = true;
        if self.app.should_quit() {
            self.loop_signal.stop();
            return;
        }
        // Draw now unless a frame callback is already on its way, which is what
        // throttles us to the compositor's pace. Waiting for a callback that was
        // never requested is how the menu used to go silent after its first draw.
        if !self.frame_pending {
            let qh = self.qh.clone();
            self.draw(&qh);
        }
    }

    /// Lays the UI out into the cell grid, paints the changed cells and presents.
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        if !self.configured {
            return;
        }
        let app = &self.app;
        if self.terminal.draw(|frame| app.view(frame)).is_err() {
            return;
        }

        let stride = self.width as i32 * 4;
        let (width, height) = (self.width as i32, self.height as i32);
        let mut fresh_buffer = false;
        let buffer = match &mut self.buffer {
            Some(buffer) => buffer,
            slot @ None => {
                let (buffer, _) =
                    match self
                        .pool
                        .create_buffer(width, height, stride, wl_shm::Format::Argb8888)
                    {
                        Ok(pair) => pair,
                        Err(err) => {
                            tracing::warn!(%err, "failed to create a buffer");
                            return;
                        }
                    };
                fresh_buffer = true;
                slot.insert(buffer)
            }
        };

        let canvas = match self.pool.canvas(buffer) {
            Some(canvas) => canvas,
            None => {
                // The compositor still holds the last buffer, so draw into a second one.
                match self
                    .pool
                    .create_buffer(width, height, stride, wl_shm::Format::Argb8888)
                {
                    Ok((second, canvas)) => {
                        *buffer = second;
                        fresh_buffer = true;
                        canvas
                    }
                    Err(err) => {
                        tracing::warn!(%err, "failed to create a second buffer");
                        return;
                    }
                }
            }
        };

        // A buffer we have never drawn into holds no history, so repaint every cell.
        if fresh_buffer {
            self.terminal.backend_mut().grid.mark_all_dirty();
        }

        let grid = &self.terminal.backend().grid;
        let mut surface = Surface::new(canvas, stride as usize, self.width, self.height);
        let damage = paint::paint(grid, &mut self.font, &mut surface);
        self.terminal.backend_mut().grid.clear_dirty();

        let wl_surface = self.shell.wl_surface();
        for rect in &damage {
            wl_surface.damage_buffer(rect.x, rect.y, rect.width, rect.height);
        }
        wl_surface.frame(qh, FrameCallbackData(wl_surface.clone()));
        self.frame_pending = true;
        if let Err(err) = buffer.attach_to(wl_surface) {
            tracing::warn!(%err, "failed to attach the buffer");
            return;
        }
        wl_surface.commit();
        self.needs_redraw = false;
    }

    fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(self.metrics.width);
        let height = height.max(self.metrics.height);
        if (width, height) == (self.width, self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.buffer = None;

        let (cols, rows) = self.terminal.backend_mut().resize_to_pixels(width, height);
        let _ = self
            .terminal
            .resize(ratatui::layout::Rect::new(0, 0, cols, rows));
        self.app.update(Event::Resize(cols, rows));
        self.needs_redraw = true;
    }

    /// A configure arrived, from either kind of surface: take its size (or
    /// keep ours when it leaves the choice to us) and draw.
    fn configured(&mut self, qh: &QueueHandle<Self>, size: Option<(u32, u32)>) {
        let (width, height) = size.unwrap_or((self.width, self.height));
        self.configured = true;
        self.resize(width, height);
        self.needs_redraw = true;
        self.draw(qh);
    }

    /// Fits the overlay to a screen whose size has become known.
    fn fit_overlay(&mut self, output: &wl_output::WlOutput) {
        let Shell::Overlay { layer, requested } = &mut self.shell else {
            return;
        };
        let Some(info) = self.output_state.info(output) else {
            return;
        };
        let screen = info
            .logical_size
            .map(|(w, h)| (w.max(0) as u32, h.max(0) as u32))
            .or_else(|| {
                info.modes.iter().find(|mode| mode.current).map(|mode| {
                    (
                        mode.dimensions.0.max(0) as u32,
                        mode.dimensions.1.max(0) as u32,
                    )
                })
            });
        let size = overlay_size(&self.metrics, screen);
        if size != *requested {
            *requested = size;
            layer.set_size(size.0, size.1);
            layer.commit();
        }
    }
}

/// Whether an error means "the Wayland connection went away".
fn is_disconnect(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(err) = source {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            return matches!(
                io.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::UnexpectedEof
            );
        }
        source = err.source();
    }
    false
}

/// Maps an xkb key event to the launcher's own key type.
fn translate_key(event: &KeyEvent, mods: Mods, repeat: bool) -> Option<Key> {
    let code = match event.keysym {
        Keysym::Return | Keysym::KP_Enter => KeyCode::Enter,
        Keysym::Escape => KeyCode::Esc,
        Keysym::BackSpace => KeyCode::Backspace,
        Keysym::Tab => KeyCode::Tab,
        Keysym::Up => KeyCode::Up,
        Keysym::Down => KeyCode::Down,
        Keysym::Left => KeyCode::Left,
        Keysym::Right => KeyCode::Right,
        Keysym::Home => KeyCode::Home,
        Keysym::End => KeyCode::End,
        Keysym::Page_Up => KeyCode::PageUp,
        Keysym::Page_Down => KeyCode::PageDown,
        Keysym::Delete => KeyCode::Delete,
        _ => {
            let ch = event
                .utf8
                .as_ref()
                .and_then(|text| text.chars().next())
                .filter(|ch| !ch.is_control())?;
            KeyCode::Char(ch)
        }
    };
    Some(Key { code, mods, repeat })
}

impl CompositorHandler for Launcher {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
        // Integer scaling support arrives with the fractional-scale work (design §10).
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.frame_pending = false;
        if self.needs_redraw {
            self.draw(qh);
        }
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl WindowHandler for Launcher {
    fn request_close(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _window: &Window) {
        self.handle(Event::Closed);
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        let size = match configure.new_size {
            (Some(width), Some(height)) => Some((width.get(), height.get())),
            _ => None,
        };
        self.configured(qh, size);
    }
}

impl LayerShellHandler for Launcher {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        // The compositor closed the overlay, e.g. its binding was pressed again.
        self.handle(Event::Closed);
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        // Zero on an axis means "your choice": keep the size we asked for.
        let size = match configure.new_size {
            (0, _) | (_, 0) => None,
            size => Some(size),
        };
        self.configured(qh, size);
    }
}

impl SeatHandler for Launcher {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard if self.keyboard.is_none() => {
                // Wayland clients handle key repeat themselves; SCTK does it for us.
                match self.seat_state.get_keyboard_with_repeat(
                    qh,
                    &seat,
                    None,
                    self.loop_handle.clone(),
                    Box::new(|state: &mut Self, _kbd, event| {
                        // This callback only ever sees repeats.
                        if let Some(key) = translate_key(&event, state.modifiers, true) {
                            state.handle(Event::Key(key));
                        }
                    }),
                ) {
                    Ok(keyboard) => self.keyboard = Some(keyboard),
                    Err(err) => tracing::warn!(%err, "no keyboard"),
                }
            }
            Capability::Pointer if self.pointer.is_none() => {
                match self.seat_state.get_pointer(qh, &seat) {
                    Ok(pointer) => self.pointer = Some(pointer),
                    Err(err) => tracing::warn!(%err, "no pointer"),
                }
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard => {
                if let Some(keyboard) = self.keyboard.take() {
                    keyboard.release();
                }
            }
            Capability::Pointer => {
                if let Some(pointer) = self.pointer.take() {
                    pointer.release();
                }
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {
    }
}

impl KeyboardHandler for Launcher {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        self.modifiers = Mods::default();
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        if let Some(key) = translate_key(&event, self.modifiers, false) {
            self.handle(Event::Key(key));
        }
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        if let Some(key) = translate_key(&event, self.modifiers, true) {
            self.handle(Event::Key(key));
        }
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _raw: RawModifiers,
        _layout: u32,
    ) {
        self.modifiers = Mods {
            ctrl: modifiers.ctrl,
            alt: modifiers.alt,
            shift: modifiers.shift,
        };
    }
}

impl PointerHandler for Launcher {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let (col, row) = (
                (event.position.0 as u32 / self.metrics.width.max(1)) as u16,
                (event.position.1 as u32 / self.metrics.height.max(1)) as u16,
            );
            let kind = match event.kind {
                PointerEventKind::Press { .. } => MouseKind::Press,
                PointerEventKind::Axis { vertical, .. } if vertical.absolute != 0.0 => {
                    if vertical.absolute < 0.0 {
                        MouseKind::ScrollUp
                    } else {
                        MouseKind::ScrollDown
                    }
                }
                _ => continue,
            };
            self.handle(Event::Mouse { col, row, kind });
        }
    }
}

impl ShmHandler for Launcher {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl OutputHandler for Launcher {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.fit_overlay(&output);
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.fit_overlay(&output);
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl ProvidesRegistryState for Launcher {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

delegate_registry!(Launcher);
delegate_dispatch2!(Launcher);

#[cfg(test)]
mod tests {
    use super::*;

    fn key_event(keysym: Keysym, utf8: Option<&str>) -> KeyEvent {
        KeyEvent {
            time: 0,
            raw_code: 0,
            keysym,
            utf8: utf8.map(str::to_string),
        }
    }

    #[test]
    fn navigation_keys_map_to_the_shared_key_type() {
        let mods = Mods::default();
        assert_eq!(
            translate_key(&key_event(Keysym::Escape, None), mods, false).map(|key| key.code),
            Some(KeyCode::Esc)
        );
        assert_eq!(
            translate_key(&key_event(Keysym::Page_Down, None), mods, false).map(|key| key.code),
            Some(KeyCode::PageDown)
        );
    }

    #[test]
    fn repeats_are_marked_so_the_app_can_ignore_them() {
        let press =
            translate_key(&key_event(Keysym::Return, None), Mods::default(), false).unwrap();
        let repeat =
            translate_key(&key_event(Keysym::Return, None), Mods::default(), true).unwrap();
        assert!(!press.repeat);
        assert!(repeat.repeat);
        assert!(press.acts_on_repeat());
        assert!(
            !repeat.acts_on_repeat(),
            "Enter must not repeat into the next screen"
        );
    }

    #[test]
    fn the_arrow_keys_move_the_selection() {
        let mods = Mods::default();
        for (keysym, expected) in [
            (Keysym::Up, KeyCode::Up),
            (Keysym::Down, KeyCode::Down),
            (Keysym::Left, KeyCode::Left),
            (Keysym::Right, KeyCode::Right),
        ] {
            let key = translate_key(&key_event(keysym, None), mods, false)
                .unwrap_or_else(|| panic!("{keysym:?} should translate"));
            assert_eq!(key.code, expected);
        }
    }

    #[test]
    fn navigation_keys_arrive_even_with_a_stray_utf8_payload() {
        // Some keymaps report a control character alongside Enter or Escape;
        // the keysym must win, or Enter would be treated as text.
        let key = translate_key(
            &key_event(Keysym::Return, Some("\r")),
            Mods::default(),
            false,
        )
        .unwrap();
        assert_eq!(key.code, KeyCode::Enter);
    }

    #[test]
    fn typed_text_comes_from_utf8() {
        let key = translate_key(&key_event(Keysym::a, Some("a")), Mods::default(), false).unwrap();
        assert_eq!(key.code, KeyCode::Char('a'));
        assert_eq!(key.typed(), Some('a'));
    }

    #[test]
    fn the_overlay_keeps_its_size_on_a_big_screen_and_shrinks_on_a_small_one() {
        let metrics = CellMetrics {
            width: 10,
            height: 20,
            baseline: 16,
            underline: 18,
        };
        let usual = (OVERLAY_COLS * 10, OVERLAY_ROWS * 20);
        assert_eq!(overlay_size(&metrics, None), usual);
        assert_eq!(overlay_size(&metrics, Some((1920, 1080))), usual);
        // 800x400: wide enough for every column, but only 20 rows less a margin of 2.
        assert_eq!(overlay_size(&metrics, Some((800, 400))), (640, 360));
        // Never smaller than something usable, even on a silly screen.
        assert_eq!(overlay_size(&metrics, Some((50, 50))), (200, 160));
    }

    #[test]
    fn control_characters_are_ignored() {
        // Ctrl+C arrives as a control character; it is not text to type into a filter.
        assert!(
            translate_key(&key_event(Keysym::c, Some("\u{3}")), Mods::default(), false).is_none()
        );
    }
}

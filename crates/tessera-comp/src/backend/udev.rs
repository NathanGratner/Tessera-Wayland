//! Real session backend: Tessera driving the hardware from a TTY (design §3.2).
//!
//! Modelled on Smithay's `anvil` udev backend, deliberately simplified:
//!
//! - **One GPU.** anvil carries a `GpuManager` so a display connected to one
//!   card can be rendered by another. Tessera renders on the primary GPU and
//!   says so if a device cannot be used, which is a laptop's reality and a
//!   tenth of the code.
//! - **No DRM leases and no syncobj.** VR headsets and explicit sync are not
//!   in v0.1.
//! - **Connectors are scanned here** rather than with `smithay-drm-extras`,
//!   whose `libdisplay-info` dependency does not build on current Arch at the
//!   pinned Smithay tag (NOTES, S2). Scanning is 40 lines.
//!
//! The rendering loop follows anvil: render, queue, and on the vblank render
//! again; when a frame produced no damage, a timer retries about one refresh
//! later, so nothing spins.

use std::{collections::HashMap, time::Duration};

use anyhow::{Context as _, anyhow};
use smithay::{
    backend::renderer::utils::CommitCounter,
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
            compositor::FrameFlags,
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            ImportDma,
            element::{
                Id, Kind, solid::SolidColorRenderElement, surface::WaylandSurfaceRenderElement,
            },
            gles::GlesRenderer,
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu},
    },
    desktop::space::{SpaceRenderElements, space_render_elements},
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            EventLoop, RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{Device as _, ModeTypeFlags, connector, crtc},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::backend::GlobalId,
    },
    utils::{DeviceFd, Rectangle},
    wayland::dmabuf::{DmabufFeedbackBuilder, DmabufGlobal, DmabufState},
};

use crate::state::Tessera;

/// Formats a scan-out buffer may use, best first.
const COLOR_FORMATS: &[Fourcc] = &[
    Fourcc::Abgr2101010,
    Fourcc::Argb2101010,
    Fourcc::Abgr8888,
    Fourcc::Argb8888,
];

/// The desktop background, matching the nested backend.
const BACKGROUND: [f32; 4] = [0.08, 0.10, 0.14, 1.0];

/// Side of the fallback pointer square, in pixels.
const CURSOR_SIZE: i32 = 12;

/// Which device to use, when the automatic choice is wrong.
const DEVICE_ENV: &str = "TESSERA_DRM_DEVICE";

type Allocator = GbmAllocator<DrmDeviceFd>;
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;
type Manager = DrmOutputManager<Allocator, Exporter, (), DrmDeviceFd>;
type Surface = DrmOutput<Allocator, Exporter, (), DrmDeviceFd>;

smithay::render_elements! {
    /// Everything drawn on a screen: the windows, and the pointer on top.
    ///
    /// Fixed to [`GlesRenderer`]: with one GPU there is only ever one renderer,
    /// and a generic parameter would have to carry Smithay's import bounds
    /// through every signature for nothing.
    pub ScreenElement<=GlesRenderer>;
    Space = SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    Cursor = SolidColorRenderElement,
}

/// One screen: a connector, its CRTC, and the output clients see.
struct Screen {
    output: Output,
    global: Option<GlobalId>,
    drm_output: Surface,
    connector: connector::Handle,
    /// A frame is on its way to the screen; the next one waits for the vblank.
    queued: bool,
}

/// Everything the real-hardware backend owns.
pub struct Udev {
    /// The seat session, which also switches VTs.
    pub session: LibSeatSession,
    /// The GPU in use.
    node: DrmNode,
    renderer: GlesRenderer,
    manager: Manager,
    screens: HashMap<crtc::Handle, Screen>,
    /// `linux-dmabuf`, so clients can render on the GPU and hand us buffers.
    pub dmabuf: Option<(DmabufState, DmabufGlobal)>,
    /// False between `PauseSession` and `ActivateSession` (another VT is up).
    active: bool,
    /// Identifies the pointer element across frames, for damage tracking.
    cursor_id: Id,
    _drm_token: RegistrationToken,
}

impl Udev {
    /// Whether a buffer can be imported for rendering; the dmabuf handler asks.
    pub fn can_import(&mut self, dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf) -> bool {
        self.renderer.import_dmabuf(dmabuf, None).is_ok()
    }
}

/// Starts the compositor on real hardware. Only ever called from a TTY.
pub fn init(
    event_loop: &mut EventLoop<'static, Tessera>,
    state: &mut Tessera,
) -> anyhow::Result<()> {
    let (session, notifier) = LibSeatSession::new().context(
        "could not join a seat session (is this running from a TTY, with seatd or logind?)",
    )?;
    let seat_name = session.seat();
    tracing::info!(seat = seat_name, "session opened");

    let (node, path) = choose_gpu(&seat_name)?;
    tracing::info!(gpu = %node, path = %path.display(), "using GPU");

    let mut session_handle = session.clone();
    let fd = session_handle
        .open(
            &path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|err| anyhow!("cannot open {}: {err}", path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (drm, drm_notifier) =
        DrmDevice::new(fd.clone(), true).context("cannot use the GPU's display controller")?;
    let gbm = GbmDevice::new(fd).context("cannot create a GBM device")?;

    // SAFETY: the display is kept alive by the context, which the renderer owns.
    let egl_display =
        unsafe { EGLDisplay::new(gbm.clone()) }.context("cannot open an EGL display")?;
    let context = EGLContext::new(&egl_display).context("cannot create an EGL context")?;
    // SAFETY: the context is current on this thread only, and the compositor is single-threaded.
    let renderer = unsafe { GlesRenderer::new(context) }.context("cannot create a GL renderer")?;

    let render_formats = renderer.egl_context().dmabuf_render_formats().clone();
    let manager = DrmOutputManager::new(
        drm,
        GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        ),
        GbmFramebufferExporter::new(
            gbm.clone(),
            node.node_with_type(NodeType::Render).and_then(Result::ok),
        ),
        Some(gbm),
        COLOR_FORMATS.iter().copied(),
        render_formats,
    );

    let drm_token = event_loop
        .handle()
        .insert_source(drm_notifier, move |event, _, state| match event {
            DrmEvent::VBlank(crtc) => state.on_vblank(crtc),
            DrmEvent::Error(err) => tracing::warn!(%err, "DRM error"),
        })
        .map_err(|err| anyhow!("cannot watch the GPU: {}", err.error))?;

    state.udev = Some(Box::new(Udev {
        session: session.clone(),
        node,
        renderer,
        manager,
        screens: HashMap::new(),
        dmabuf: None,
        active: true,
        cursor_id: Id::new(),
        _drm_token: drm_token,
    }));

    init_input(event_loop, state, &session, &seat_name)?;
    init_session_events(event_loop, notifier, session)?;
    init_hotplug(event_loop, &seat_name)?;

    state.scan_connectors();
    state.init_dmabuf();
    if state.space.outputs().next().is_none() {
        tracing::warn!("no screen is connected; Tessera is running with nothing to show");
    }
    Ok(())
}

/// The GPU to render on: `TESSERA_DRM_DEVICE`, else the seat's primary GPU,
/// else the first one that can be opened.
fn choose_gpu(seat: &str) -> anyhow::Result<(DrmNode, std::path::PathBuf)> {
    if let Some(path) = std::env::var_os(DEVICE_ENV) {
        let path = std::path::PathBuf::from(path);
        let node = DrmNode::from_path(&path)
            .map_err(|err| anyhow!("{DEVICE_ENV} is not a DRM device: {err}"))?;
        return Ok((node, path));
    }

    let primary = primary_gpu(seat)
        .context("cannot ask udev for the primary GPU")?
        .and_then(|path| DrmNode::from_path(&path).ok().map(|node| (node, path)));
    if let Some(found) = primary {
        return Ok(found);
    }

    all_gpus(seat)
        .context("cannot list GPUs")?
        .into_iter()
        .find_map(|path| DrmNode::from_path(&path).ok().map(|node| (node, path)))
        .ok_or_else(|| anyhow!("no GPU found on seat {seat}"))
}

/// libinput: real keyboards, mice and touchpads.
fn init_input(
    event_loop: &mut EventLoop<'static, Tessera>,
    state: &mut Tessera,
    session: &LibSeatSession,
    seat_name: &str,
) -> anyhow::Result<()> {
    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(seat_name)
        .map_err(|()| anyhow!("cannot take input devices for seat {seat_name}"))?;
    state.libinput = Some(libinput.clone());

    event_loop
        .handle()
        .insert_source(LibinputInputBackend::new(libinput), |event, _, state| {
            state.process_input_event(event);
        })
        .map_err(|err| anyhow!("cannot watch input devices: {}", err.error))?;
    Ok(())
}

/// VT switches: the session goes away and comes back, and so must the GPU.
fn init_session_events(
    event_loop: &mut EventLoop<'static, Tessera>,
    notifier: smithay::backend::session::libseat::LibSeatSessionNotifier,
    _session: LibSeatSession,
) -> anyhow::Result<()> {
    event_loop
        .handle()
        .insert_source(notifier, move |event, _, state| match event {
            SessionEvent::PauseSession => {
                tracing::info!("session paused: another VT has the screen");
                if let Some(libinput) = state.libinput.as_mut() {
                    libinput.suspend();
                }
                if let Some(udev) = state.udev.as_mut() {
                    udev.active = false;
                    udev.manager.pause();
                }
            }
            SessionEvent::ActivateSession => {
                tracing::info!("session resumed");
                if let Some(libinput) = state.libinput.as_mut()
                    && let Err(err) = libinput.resume()
                {
                    tracing::warn!(?err, "cannot take input devices back");
                }
                let crtcs: Vec<crtc::Handle> = match state.udev.as_mut() {
                    Some(udev) => {
                        udev.active = true;
                        if let Err(err) = udev.manager.activate(false) {
                            tracing::warn!(%err, "cannot take the GPU back");
                        }
                        udev.screens.keys().copied().collect()
                    }
                    None => Vec::new(),
                };
                // Connectors may have changed while we were away.
                state.scan_connectors();
                for crtc in crtcs {
                    if let Some(screen) = state.udev.as_mut().and_then(|u| u.screens.get_mut(&crtc))
                    {
                        screen.queued = false;
                    }
                    state.render_screen(crtc);
                }
            }
        })
        .map_err(|err| anyhow!("cannot watch the session: {}", err.error))?;
    Ok(())
}

/// Monitors being plugged in and out arrive as udev "changed" events.
fn init_hotplug(
    event_loop: &mut EventLoop<'static, Tessera>,
    seat_name: &str,
) -> anyhow::Result<()> {
    let udev_backend = UdevBackend::new(seat_name).context("cannot watch for device changes")?;
    event_loop
        .handle()
        .insert_source(udev_backend, |event, _, state| {
            let device_id = match event {
                UdevEvent::Added { device_id, .. } => device_id,
                UdevEvent::Changed { device_id } => device_id,
                UdevEvent::Removed { device_id } => device_id,
            };
            let ours = state
                .udev
                .as_ref()
                .is_some_and(|udev| udev.node.dev_id() == device_id);
            if ours {
                state.scan_connectors();
            }
        })
        .map_err(|err| anyhow!("cannot watch for device changes: {}", err.error))?;
    Ok(())
}

impl Tessera {
    /// Advertises `linux-dmabuf`, so GL clients render on the GPU instead of
    /// falling back to software (the S1 finding).
    fn init_dmabuf(&mut self) {
        let Some(udev) = self.udev.as_mut() else {
            return;
        };
        let formats = udev.renderer.dmabuf_formats();
        let feedback = match DmabufFeedbackBuilder::new(udev.node.dev_id(), formats).build() {
            Ok(feedback) => feedback,
            Err(err) => {
                tracing::warn!(%err, "no dmabuf support; GL clients will render in software");
                return;
            }
        };
        let mut dmabuf_state = DmabufState::new();
        let global = dmabuf_state
            .create_global_with_default_feedback::<Tessera>(&self.display_handle, &feedback);
        udev.dmabuf = Some((dmabuf_state, global));
        tracing::info!("linux-dmabuf ready");
    }

    /// Brings the set of screens in line with what is plugged in.
    ///
    /// Replaces `smithay-drm-extras`' `DrmScanner`: for every connected
    /// connector without a screen, find a CRTC no other screen is using.
    pub(crate) fn scan_connectors(&mut self) {
        let Some(udev) = self.udev.as_mut() else {
            return;
        };
        // While another VT has the screen, the GPU is not ours to set modes on:
        // a monitor plugged in now fails with "device is currently paused".
        // Resuming rescans, so it is picked up then.
        if !udev.active {
            tracing::debug!("not scanning screens while the session is paused");
            return;
        }
        let device = udev.manager.device();
        let Ok(resources) = device.resource_handles() else {
            tracing::warn!("cannot read the GPU's connectors");
            return;
        };

        let mut connected: Vec<(connector::Handle, connector::Info)> = Vec::new();
        for handle in resources.connectors() {
            // `false`: use the cached state rather than forcing a probe of every connector.
            if let Ok(info) = device.get_connector(*handle, false)
                && info.state() == connector::State::Connected
            {
                connected.push((*handle, info));
            }
        }

        // Screens whose connector went away.
        let gone: Vec<crtc::Handle> = udev
            .screens
            .iter()
            .filter(|(_, screen)| {
                !connected
                    .iter()
                    .any(|(handle, _)| *handle == screen.connector)
            })
            .map(|(crtc, _)| *crtc)
            .collect();
        for crtc in gone {
            self.remove_screen(crtc);
        }

        for (handle, info) in connected {
            let Some(udev) = self.udev.as_mut() else {
                return;
            };
            if udev
                .screens
                .values()
                .any(|screen| screen.connector == handle)
            {
                continue;
            }
            let Some(crtc) = free_crtc(udev, &info) else {
                tracing::warn!(connector = %name_of(&info), "no free CRTC for this screen");
                continue;
            };
            if let Err(err) = self.add_screen(handle, info, crtc) {
                tracing::warn!(error = %format!("{err:#}"), "cannot set up a screen");
            }
        }

        self.apply_layout();
    }

    /// Sets up one screen and starts drawing on it.
    fn add_screen(
        &mut self,
        connector: connector::Handle,
        info: connector::Info,
        crtc: crtc::Handle,
    ) -> anyhow::Result<()> {
        let udev = self.udev.as_mut().expect("called with a udev backend");
        let name = name_of(&info);

        let choice = ModeChoice::parse(&self.settings.text("display.mode")).unwrap_or_else(|err| {
            tracing::warn!(%err, "display.mode is not usable; taking the screen's preference");
            ModeChoice::Preferred
        });
        let infos: Vec<ModeInfo> = info.modes().iter().map(mode_info).collect();
        let mode_index =
            choose_mode(&infos, &choice).ok_or_else(|| anyhow!("{name} offers no modes"))?;
        let drm_mode = info.modes()[mode_index];
        let wl_mode = Mode::from(drm_mode);

        let (width_mm, height_mm) = info.size().unwrap_or((0, 0));
        let output = Output::new(
            name.clone(),
            PhysicalProperties {
                size: (width_mm as i32, height_mm as i32).into(),
                subpixel: Subpixel::Unknown,
                make: "Unknown".into(),
                model: name.clone(),
            },
        );
        let global = output.create_global::<Tessera>(&self.display_handle);
        output.set_preferred(wl_mode);

        // Screens sit side by side, in the order they were found.
        let x = self
            .space
            .outputs()
            .filter_map(|other| self.space.output_geometry(other))
            .map(|geometry| geometry.loc.x + geometry.size.w)
            .max()
            .unwrap_or(0);
        output.change_current_state(Some(wl_mode), None, None, Some((x, 0).into()));

        let planes = udev.manager.device().planes(&crtc).ok();
        let drm_output = udev
            .manager
            .initialize_output::<_, ScreenElement>(
                crtc,
                drm_mode,
                &[connector],
                &output,
                planes,
                &mut udev.renderer,
                &DrmOutputRenderElements::default(),
            )
            .map_err(|err| anyhow!("cannot drive {name}: {err}"))?;

        udev.screens.insert(
            crtc,
            Screen {
                output: output.clone(),
                global: Some(global),
                drm_output,
                connector,
                queued: false,
            },
        );
        self.space.map_output(&output, (x, 0));
        tracing::info!(
            screen = name,
            width = wl_mode.size.w,
            height = wl_mode.size.h,
            refresh = wl_mode.refresh,
            "screen ready"
        );

        self.render_screen(crtc);
        Ok(())
    }

    /// Drops a screen whose monitor was unplugged.
    fn remove_screen(&mut self, crtc: crtc::Handle) {
        let Some(udev) = self.udev.as_mut() else {
            return;
        };
        let Some(mut screen) = udev.screens.remove(&crtc) else {
            return;
        };
        tracing::info!(screen = screen.output.name(), "screen disconnected");
        if let Some(global) = screen.global.take() {
            self.display_handle.remove_global::<Tessera>(global);
        }
        self.space.unmap_output(&screen.output);
    }

    /// Draws one screen and queues the frame.
    pub(crate) fn render_screen(&mut self, crtc: crtc::Handle) {
        let pointer = self.pointer_location;
        let Some(udev) = self.udev.as_mut() else {
            return;
        };
        if !udev.active {
            return;
        }
        let Some(screen) = udev.screens.get_mut(&crtc) else {
            return;
        };
        if screen.queued {
            return;
        }
        let output = screen.output.clone();

        let mut elements: Vec<ScreenElement> = Vec::new();
        // The pointer goes on top, so it is drawn first.
        let geometry = self
            .space
            .output_geometry(&output)
            .unwrap_or_else(|| Rectangle::from_size((0, 0).into()));
        if geometry.to_f64().contains(pointer) {
            // Scale is 1 for now (integer scaling only, design §10), so logical
            // and physical pixels are the same number.
            let location = (pointer - geometry.loc.to_f64()).to_i32_round::<i32>();
            let position: smithay::utils::Point<i32, smithay::utils::Physical> =
                (location.x, location.y).into();
            elements.push(ScreenElement::Cursor(SolidColorRenderElement::new(
                udev.cursor_id.clone(),
                Rectangle::new(position, (CURSOR_SIZE, CURSOR_SIZE).into()),
                CommitCounter::default(),
                [0.9, 0.9, 0.9, 1.0],
                Kind::Cursor,
            )));
        }
        match space_render_elements(&mut udev.renderer, [&self.space], &output, 1.0) {
            Ok(windows) => elements.extend(windows.into_iter().map(ScreenElement::Space)),
            Err(err) => {
                tracing::warn!(%err, "no mode for this screen yet");
                return;
            }
        }

        let rendered = screen
            .drm_output
            .render_frame(
                &mut udev.renderer,
                &elements,
                BACKGROUND,
                FrameFlags::DEFAULT,
            )
            .map(|result| !result.is_empty);

        match rendered {
            Ok(true) => match screen.drm_output.queue_frame(()) {
                Ok(()) => screen.queued = true,
                Err(err) => {
                    tracing::warn!(%err, "cannot queue a frame");
                    self.retry_render(crtc);
                }
            },
            // Nothing changed: look again in about one refresh.
            Ok(false) => self.retry_render(crtc),
            Err(err) => {
                tracing::warn!(%err, "cannot draw a frame");
                self.retry_render(crtc);
            }
        }

        self.send_frames(&output);
    }

    /// Asks for another attempt at drawing this screen, one refresh from now.
    fn retry_render(&mut self, crtc: crtc::Handle) {
        let refresh = self
            .udev
            .as_ref()
            .and_then(|udev| udev.screens.get(&crtc))
            .and_then(|screen| screen.output.current_mode())
            .map(|mode| mode.refresh.max(1) as u64)
            .unwrap_or(60_000);
        let delay = Duration::from_micros(1_000_000_000 / refresh);
        let timer = Timer::from_duration(delay);
        if let Err(err) = self.loop_handle.insert_source(timer, move |_, _, state| {
            state.render_screen(crtc);
            TimeoutAction::Drop
        }) {
            tracing::warn!(error = %err.error, "cannot schedule the next frame");
        }
    }

    /// A frame reached the screen: tell clients, then draw the next one.
    pub(crate) fn on_vblank(&mut self, crtc: crtc::Handle) {
        let Some(udev) = self.udev.as_mut() else {
            return;
        };
        let Some(screen) = udev.screens.get_mut(&crtc) else {
            return;
        };
        screen.queued = false;
        if let Err(err) = screen.drm_output.frame_submitted() {
            tracing::warn!(%err, "vblank for a frame we did not queue");
        }
        let output = screen.output.clone();
        self.send_frames(&output);
        self.render_screen(crtc);
    }

    /// Lets clients on this screen draw their next frame.
    fn send_frames(&self, output: &Output) {
        let time = self.start_time.elapsed();
        for window in self.space.elements() {
            window.send_frame(output, time, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
    }

    /// Redraws every screen, e.g. after the layout changed.
    pub(crate) fn render_all(&mut self) {
        let crtcs: Vec<crtc::Handle> = match self.udev.as_ref() {
            Some(udev) => udev.screens.keys().copied().collect(),
            None => return,
        };
        for crtc in crtcs {
            self.render_screen(crtc);
        }
    }

    /// Re-reads `display.mode` and changes any screen that is on another mode.
    ///
    /// Called when the configuration is applied, so changing the resolution
    /// takes effect without unplugging anything.
    pub(crate) fn apply_display_modes(&mut self) {
        let wanted = self.settings.text("display.mode");
        let Some(udev) = self.udev.as_ref() else {
            return;
        };
        if udev.screens.is_empty() {
            return;
        }
        let choice = match ModeChoice::parse(&wanted) {
            Ok(choice) => choice,
            Err(err) => {
                tracing::warn!(%err, "display.mode is not usable; leaving the screens alone");
                return;
            }
        };

        let screens: Vec<(crtc::Handle, connector::Handle)> = udev
            .screens
            .iter()
            .map(|(crtc, screen)| (*crtc, screen.connector))
            .collect();
        let mut changed = false;

        for (crtc, connector) in screens {
            let Some(udev) = self.udev.as_mut() else {
                return;
            };
            let Ok(info) = udev.manager.device().get_connector(connector, false) else {
                continue;
            };
            let infos: Vec<ModeInfo> = info.modes().iter().map(mode_info).collect();
            let Some(index) = choose_mode(&infos, &choice) else {
                continue;
            };
            let drm_mode = info.modes()[index];
            let wl_mode = Mode::from(drm_mode);

            // Split the borrow: the renderer and the screen are both needed.
            let Udev {
                renderer, screens, ..
            } = &mut **udev;
            let Some(screen) = screens.get_mut(&crtc) else {
                continue;
            };
            if screen.output.current_mode() == Some(wl_mode) {
                continue;
            }
            match screen.drm_output.use_mode(
                drm_mode,
                renderer,
                &DrmOutputRenderElements::<GlesRenderer, ScreenElement>::default(),
            ) {
                Ok(()) => {
                    screen
                        .output
                        .change_current_state(Some(wl_mode), None, None, None);
                    screen.queued = false;
                    changed = true;
                    tracing::info!(
                        screen = screen.output.name(),
                        width = wl_mode.size.w,
                        height = wl_mode.size.h,
                        refresh = wl_mode.refresh,
                        "screen mode changed"
                    );
                }
                Err(err) => tracing::warn!(%err, "cannot change the screen's mode"),
            }
        }

        if changed {
            self.apply_layout();
        }
    }

    /// Switches to another virtual terminal, e.g. Ctrl+Alt+F2.
    pub(crate) fn switch_vt(&mut self, vt: i32) {
        let Some(udev) = self.udev.as_mut() else {
            return;
        };
        tracing::info!(vt, "switching virtual terminal");
        if let Err(err) = udev.session.change_vt(vt) {
            tracing::warn!(%err, vt, "cannot switch virtual terminal");
        }
    }
}

/// What `display.mode` asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModeChoice {
    /// Whatever the screen says it prefers.
    Preferred,
    /// A size, and optionally a refresh rate in Hz.
    Size {
        /// Width in pixels.
        width: u16,
        /// Height in pixels.
        height: u16,
        /// Refresh rate in Hz, when the setting named one.
        refresh: Option<f32>,
    },
}

impl ModeChoice {
    /// Parses `preferred`, `1920x1080` or `1920x1080@60`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() || text.eq_ignore_ascii_case("preferred") {
            return Ok(ModeChoice::Preferred);
        }
        let (size, refresh) = match text.split_once('@') {
            Some((size, rate)) => {
                let rate: f32 = rate
                    .trim()
                    .parse()
                    .map_err(|_| format!("`{rate}` is not a refresh rate in Hz"))?;
                (size, Some(rate))
            }
            None => (text, None),
        };
        let (width, height) = size
            .split_once(['x', 'X'])
            .ok_or_else(|| format!("`{text}` is not a size; write it like 1920x1080"))?;
        Ok(ModeChoice::Size {
            width: width
                .trim()
                .parse()
                .map_err(|_| format!("`{width}` is not a width"))?,
            height: height
                .trim()
                .parse()
                .map_err(|_| format!("`{height}` is not a height"))?,
            refresh,
        })
    }
}

/// The parts of a DRM mode that choosing one looks at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeInfo {
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
    /// Refresh rate in Hz.
    pub refresh: u32,
    /// The screen says this is the one it wants.
    pub preferred: bool,
}

/// Picks the mode to drive a screen at.
///
/// An exact size wins; the closest refresh rate among those, or the highest
/// when the setting did not name one. Failing that, the largest mode that
/// fits inside the asked-for size, so a 4K monitor asked for 1920x1080 on a
/// machine that only offers 1680x1050 still gets something sensible. Failing
/// that, the screen's own preference, which is what Tessera did before the
/// setting existed.
pub fn choose_mode(modes: &[ModeInfo], choice: &ModeChoice) -> Option<usize> {
    let preferred = || {
        modes
            .iter()
            .position(|mode| mode.preferred)
            .or(if modes.is_empty() { None } else { Some(0) })
    };
    let ModeChoice::Size {
        width,
        height,
        refresh,
    } = *choice
    else {
        return preferred();
    };

    let best = |candidates: Vec<usize>| -> Option<usize> {
        candidates.into_iter().max_by(|&a, &b| {
            let (a, b) = (&modes[a], &modes[b]);
            let score = |mode: &ModeInfo| match refresh {
                // Nearest refresh rate: smaller difference is better, hence the negation.
                Some(wanted) => (
                    -((mode.refresh as f32 - wanted).abs() * 1000.0) as i64,
                    mode.width as i64 * mode.height as i64,
                ),
                None => (mode.refresh as i64, mode.width as i64 * mode.height as i64),
            };
            score(a).cmp(&score(b))
        })
    };

    let exact: Vec<usize> = (0..modes.len())
        .filter(|&i| modes[i].width == width && modes[i].height == height)
        .collect();
    if let Some(found) = best(exact) {
        return Some(found);
    }

    let fits: Vec<usize> = (0..modes.len())
        .filter(|&i| modes[i].width <= width && modes[i].height <= height)
        .collect();
    best(fits).or_else(preferred)
}

/// A CRTC this connector can use that no other screen has taken.
fn free_crtc(udev: &Udev, info: &connector::Info) -> Option<crtc::Handle> {
    let device = udev.manager.device();
    let resources = device.resource_handles().ok()?;
    let taken: Vec<crtc::Handle> = udev.screens.keys().copied().collect();

    for encoder_handle in info.encoders() {
        let Ok(encoder) = device.get_encoder(*encoder_handle) else {
            continue;
        };
        for crtc in resources.filter_crtcs(encoder.possible_crtcs()) {
            if !taken.contains(&crtc) {
                return Some(crtc);
            }
        }
    }
    None
}

/// What choosing a mode needs to know about a DRM mode.
fn mode_info(mode: &smithay::reexports::drm::control::Mode) -> ModeInfo {
    let (width, height) = mode.size();
    ModeInfo {
        width,
        height,
        refresh: mode.vrefresh(),
        preferred: mode.mode_type().contains(ModeTypeFlags::PREFERRED),
    }
}

/// The name a connector is known by, e.g. `eDP-1`.
fn name_of(info: &connector::Info) -> String {
    format!("{}-{}", info.interface().as_str(), info.interface_id())
}

/// Tells the user's systemd about `WAYLAND_DISPLAY`, and returns what was
/// there before so it can be put back.
///
/// Portals and other user services are started by systemd, which knows nothing
/// of a compositor started from a TTY. Without this, file pickers and
/// screen-sharing look for a display that, as far as they can tell, is not
/// there. The socket only exists once the compositor is up, so the session
/// script cannot do this itself.
///
/// One `systemd --user` is shared by every session the same user has open, so
/// this is borrowed rather than taken: a desktop on another VT has its own
/// display, and leaving ours behind points its services at a socket that has
/// gone. Hence the previous value, and [`restore_wayland_display`].
#[must_use = "the previous value has to be put back when the session ends"]
pub fn export_wayland_display(socket: &std::ffi::OsStr) -> Option<String> {
    let previous = user_environment("WAYLAND_DISPLAY");
    let assignment = format!("WAYLAND_DISPLAY={}", socket.to_string_lossy());
    if run_systemctl(&["--user", "set-environment", &assignment]) {
        tracing::debug!(?previous, "told systemd --user about WAYLAND_DISPLAY");
    } else {
        tracing::debug!("user services will not see WAYLAND_DISPLAY");
    }
    previous
}

/// Puts `WAYLAND_DISPLAY` back as it was before this session started.
pub fn restore_wayland_display(previous: Option<String>) {
    let restored = match &previous {
        Some(value) => {
            let assignment = format!("WAYLAND_DISPLAY={value}");
            run_systemctl(&["--user", "set-environment", &assignment])
        }
        None => run_systemctl(&["--user", "unset-environment", "WAYLAND_DISPLAY"]),
    };
    if restored {
        tracing::debug!(?previous, "put WAYLAND_DISPLAY back for systemd --user");
    }
}

/// What `systemd --user` currently has for a variable.
fn user_environment(name: &str) -> Option<String> {
    let output = std::process::Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
        .ok()?;
    environment_value(&String::from_utf8_lossy(&output.stdout), name)
}

/// Finds one variable in `systemctl show-environment` output.
fn environment_value(text: &str, name: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .find(|(key, _)| *key == name)
        // systemd quotes values that need it; nothing else strips them.
        .map(|(_, value)| value.trim_matches('"').to_string())
}

fn run_systemctl(args: &[&str]) -> bool {
    match std::process::Command::new("systemctl").args(args).status() {
        Ok(status) => status.success(),
        Err(err) => {
            tracing::debug!(%err, "no systemctl");
            false
        }
    }
}

/// How large the log may grow before the previous one is rotated away.
const LOG_ROTATE_BYTES: u64 = 2 * 1024 * 1024;

/// Opens the session log for appending, rotating it when it gets large.
///
/// Appending rather than truncating: when a session dies, the log of the run
/// that died is the only evidence, and starting the next run must not erase
/// it. One old file is kept, as `tessera.log.1`.
pub fn open_log(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > LOG_ROTATE_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

/// Whether this process looks like it is on a TTY rather than inside a desktop.
pub fn looks_like_a_tty() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none()
}

/// Where the log goes when Tessera is the session: `$XDG_STATE_HOME/tessera/tessera.log`.
pub fn log_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/state"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    base.join("tessera").join("tessera.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes() -> Vec<ModeInfo> {
        // A 4K monitor as it really presents itself: preferred is the big one.
        vec![
            ModeInfo {
                width: 3840,
                height: 2160,
                refresh: 60,
                preferred: true,
            },
            ModeInfo {
                width: 3840,
                height: 2160,
                refresh: 30,
                preferred: false,
            },
            ModeInfo {
                width: 1920,
                height: 1080,
                refresh: 120,
                preferred: false,
            },
            ModeInfo {
                width: 1920,
                height: 1080,
                refresh: 60,
                preferred: false,
            },
            ModeInfo {
                width: 1680,
                height: 1050,
                refresh: 60,
                preferred: false,
            },
        ]
    }

    #[test]
    fn a_resolution_is_parsed_from_the_setting() {
        assert_eq!(
            ModeChoice::parse("preferred").unwrap(),
            ModeChoice::Preferred
        );
        assert_eq!(ModeChoice::parse("").unwrap(), ModeChoice::Preferred);
        assert_eq!(
            ModeChoice::parse("1920x1080").unwrap(),
            ModeChoice::Size {
                width: 1920,
                height: 1080,
                refresh: None
            }
        );
        assert_eq!(
            ModeChoice::parse(" 1920X1080@59.94 ").unwrap(),
            ModeChoice::Size {
                width: 1920,
                height: 1080,
                refresh: Some(59.94)
            }
        );
        assert!(ModeChoice::parse("huge").is_err());
        assert!(ModeChoice::parse("1920x1080@fast").is_err());
    }

    #[test]
    fn preferred_takes_what_the_screen_asks_for() {
        assert_eq!(choose_mode(&modes(), &ModeChoice::Preferred), Some(0));
        assert_eq!(choose_mode(&[], &ModeChoice::Preferred), None);
    }

    #[test]
    fn an_exact_size_wins_at_the_highest_refresh() {
        let choice = ModeChoice::parse("1920x1080").unwrap();
        assert_eq!(choose_mode(&modes(), &choice), Some(2), "120 Hz, not 60");
    }

    #[test]
    fn a_named_refresh_rate_picks_the_nearest() {
        let choice = ModeChoice::parse("1920x1080@60").unwrap();
        assert_eq!(choose_mode(&modes(), &choice), Some(3));
        let choice = ModeChoice::parse("3840x2160@30").unwrap();
        assert_eq!(choose_mode(&modes(), &choice), Some(1));
    }

    #[test]
    fn a_size_the_screen_lacks_falls_back_to_the_largest_that_fits() {
        let choice = ModeChoice::parse("1920x1200").unwrap();
        assert_eq!(
            choose_mode(&modes(), &choice),
            Some(2),
            "1920x1080 fits inside it"
        );

        // Nothing fits: the screen's own preference is better than nothing.
        let choice = ModeChoice::parse("640x480").unwrap();
        assert_eq!(choose_mode(&modes(), &choice), Some(0));
    }

    #[test]
    fn a_variable_is_read_out_of_systemctl_output() {
        let text = "LANG=en_GB.UTF-8\nWAYLAND_DISPLAY=wayland-0\nPATH=/usr/bin\n";
        assert_eq!(
            environment_value(text, "WAYLAND_DISPLAY"),
            Some("wayland-0".to_string())
        );
        assert_eq!(environment_value(text, "DISPLAY"), None);
        assert_eq!(
            environment_value("WAYLAND_DISPLAY=\"wayland 1\"\n", "WAYLAND_DISPLAY"),
            Some("wayland 1".to_string()),
            "quoted values are unquoted"
        );
    }

    #[test]
    fn the_log_is_appended_to_and_rotated() {
        let dir = std::env::temp_dir().join(format!("tessera-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("tessera.log");

        for line in ["first run\n", "second run\n"] {
            let mut file = open_log(&path).unwrap();
            std::io::Write::write_all(&mut file, line.as_bytes()).unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "first run\nsecond run\n", "the earlier run survives");

        std::fs::write(&path, vec![b'x'; LOG_ROTATE_BYTES as usize + 1]).unwrap();
        open_log(&path).unwrap();
        assert!(
            path.with_extension("log.1").exists(),
            "the big one was rotated away"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            0,
            "and a fresh one started"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_log_lives_under_the_state_directory() {
        let path = log_path();
        assert!(path.ends_with("tessera/tessera.log"), "{path:?}");
        assert!(path.is_absolute(), "{path:?}");
    }

    #[test]
    fn a_desktop_session_means_we_are_not_on_a_tty() {
        // The environment of the test runner decides; check both readings agree.
        let inside_desktop =
            std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
        assert_eq!(looks_like_a_tty(), !inside_desktop);
    }
}

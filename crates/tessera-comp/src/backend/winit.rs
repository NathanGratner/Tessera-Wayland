//! Nested backend: Tessera runs as a window inside the host desktop.

use anyhow::{Context, anyhow};
use smithay::{
    backend::{
        renderer::{
            damage::OutputDamageTracker, element::surface::WaylandSurfaceRenderElement,
            gles::GlesRenderer,
        },
        winit::{self, WinitEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::EventLoop,
        winit::{dpi::LogicalSize, window::Window as WinitWindow},
    },
    utils::{Rectangle, Transform},
};

use crate::state::Tessera;

const REFRESH_MHZ: i32 = 60_000;
const BACKGROUND: [f32; 4] = [0.08, 0.10, 0.14, 1.0];

pub fn init(
    event_loop: &mut EventLoop<'static, Tessera>,
    state: &mut Tessera,
) -> anyhow::Result<()> {
    let attributes = WinitWindow::default_attributes()
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_title("Tessera (nested)")
        .with_visible(true);
    let (mut backend, winit) = winit::init_from_attributes::<GlesRenderer>(attributes)
        .map_err(|err| anyhow!("failed to open the nested window: {err}"))?;

    let mode = Mode {
        size: backend.window_size(),
        refresh: REFRESH_MHZ,
    };
    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Tessera".into(),
            model: "Nested".into(),
        },
    );
    let _global = output.create_global::<Tessera>(&state.display_handle);
    // GL renders upside down relative to the winit window.
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));

    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    event_loop
        .handle()
        .insert_source(winit, move |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                output.change_current_state(
                    Some(Mode {
                        size,
                        refresh: REFRESH_MHZ,
                    }),
                    None,
                    None,
                    None,
                );
                // The output is a different size now, so every tile must be
                // recomputed; changing the mode alone leaves them where they were.
                tracing::debug!(width = size.w, height = size.h, "nested window resized");
                state.apply_layout();
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            // The host sends no key releases to a window that has lost the
            // keyboard, so a key held while leaving (Alt, during Alt+Tab)
            // would otherwise stay down in here, and every key would then be
            // Mod+key: Return opening terminals, letters moving focus.
            WinitEvent::Focus(false) => state.release_all_keys(),
            WinitEvent::Redraw => {
                let size = backend.window_size();
                let damage = Rectangle::from_size(size);

                let rendered = backend.bind().map_err(|err| format!("{err:?}")).and_then(
                    |(renderer, mut framebuffer)| {
                        smithay::desktop::space::render_output::<
                            _,
                            WaylandSurfaceRenderElement<GlesRenderer>,
                            _,
                            _,
                        >(
                            &output,
                            renderer,
                            &mut framebuffer,
                            1.0,
                            0,
                            [&state.space],
                            &[],
                            &mut damage_tracker,
                            BACKGROUND,
                        )
                        .map(|_| ())
                        .map_err(|err| format!("{err:?}"))
                    },
                );
                match rendered {
                    Ok(()) => {
                        if let Err(err) = backend.submit(Some(&[damage])) {
                            tracing::warn!(?err, "failed to present frame");
                        }
                    }
                    Err(err) => tracing::warn!(err, "failed to render frame"),
                }

                state.send_frames(&output);

                backend.window().request_redraw();
            }
            WinitEvent::CloseRequested => {
                tracing::info!("nested window closed");
                state.loop_signal.stop();
            }
            _ => {}
        })
        .map_err(|err| anyhow!("failed to register the winit event source: {}", err.error))
        .context("nested backend setup")?;

    Ok(())
}

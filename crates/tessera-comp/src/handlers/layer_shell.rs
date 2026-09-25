//! `wlr-layer-shell`: surfaces that sit on a layer of the screen instead of
//! being tiled, such as the application overlay, launchers, bars and
//! notifications.
//!
//! A layer surface belongs to one output's [`LayerMap`](smithay::desktop::LayerMap),
//! which places it by its anchors and margins; drawing and hit-testing read the
//! same map. Keyboard focus is kept apart from window focus: see
//! [`Tessera::layer_focus`].

use smithay::{
    delegate_layer_shell,
    desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output},
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    wayland::{
        compositor::with_states,
        shell::wlr_layer::{
            KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData,
            WlrLayerShellHandler, WlrLayerShellState,
        },
    },
};

use crate::state::Tessera;

impl WlrLayerShellHandler for Tessera {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        // The output the client asked for, else the first: there is no
        // "focused output" yet, because Tessera drives one screen at a time.
        let Some(output) = output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.space.outputs().next().cloned())
        else {
            tracing::warn!(
                namespace,
                "a layer surface arrived before any screen; closing it"
            );
            surface.send_close();
            return;
        };
        tracing::debug!(namespace, ?layer, output = output.name(), "layer surface");
        let mut map = layer_map_for_output(&output);
        if let Err(err) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            tracing::warn!(?err, "could not map a layer surface");
        }
        // Nothing to draw or focus until its first commit, which sends the
        // initial configure (see `layer_commit`).
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        for output in self.space.outputs().cloned().collect::<Vec<_>>() {
            let mut map = layer_map_for_output(&output);
            let layer = map
                .layers()
                .find(|layer| layer.layer_surface() == &surface)
                .cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
            }
        }
        // No longer mapped, so this hands the keyboard back to the window.
        self.refresh_layer_focus();
        // A bar going away gives its reserved space back to the tiles.
        self.apply_layout();
    }
}

delegate_layer_shell!(Tessera);

impl Tessera {
    /// Handles a commit on a layer surface; does nothing for any other surface.
    ///
    /// The first commit is when the client has said what it wants (size,
    /// anchors, layer), so that is when it is placed and gets its initial
    /// configure. Later commits can change any of that, so it is placed again.
    pub(crate) fn layer_commit(&mut self, surface: &WlSurface) {
        let Some(output) = self.space.outputs().find(|output| {
            layer_map_for_output(output)
                .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .is_some()
        }) else {
            return;
        };
        let output = output.clone();

        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .and_then(|data| data.lock().ok())
                .is_some_and(|data| data.initial_configure_sent)
        });

        let zone_changed = {
            let mut map = layer_map_for_output(&output);
            let before = map.non_exclusive_zone();
            // Arranged before the initial configure, so the size the client
            // asked for is the size it is told.
            map.arrange();
            if !initial_configure_sent
                && let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            {
                layer.layer_surface().send_configure();
            }
            map.non_exclusive_zone() != before
        };

        self.refresh_layer_focus();
        if zone_changed {
            // A bar reserved or released space: the tiles have to move.
            self.apply_layout();
        }
    }

    /// Re-decides which layer surface, if any, holds the keyboard, and hands
    /// the keyboard to it or back to the focused window.
    ///
    /// A surface on the overlay or top layer asking for `exclusive` keyboard
    /// interactivity takes the keyboard for as long as it is mapped; the most
    /// recently mapped one wins. An `on_demand` surface keeps the keyboard
    /// only once it has been clicked (see [`Self::focus_layer`]).
    pub(crate) fn refresh_layer_focus(&mut self) {
        let exclusive = self.space.outputs().find_map(|output| {
            let map = layer_map_for_output(output);
            [Layer::Overlay, Layer::Top].into_iter().find_map(|layer| {
                map.layers_on(layer)
                    .rev()
                    .find(|surface| {
                        surface.cached_state().keyboard_interactivity
                            == KeyboardInteractivity::Exclusive
                    })
                    .cloned()
            })
        });

        let next = exclusive.or_else(|| {
            // An on-demand surface keeps the keyboard while it is still mapped
            // and still wants it.
            self.layer_focus
                .clone()
                .filter(|layer| self.layer_is_mapped(layer) && layer.can_receive_keyboard_focus())
        });
        if next != self.layer_focus {
            tracing::debug!(
                namespace = next.as_ref().map(|layer| layer.namespace().to_string()),
                "layer keyboard focus"
            );
            self.layer_focus = next;
        }
        self.update_keyboard_focus();
    }

    /// Gives the keyboard to a layer surface that was clicked, if it takes one.
    pub(crate) fn focus_layer(&mut self, layer: &LayerSurface) {
        if layer.can_receive_keyboard_focus() {
            self.layer_focus = Some(layer.clone());
        }
        self.refresh_layer_focus();
    }

    /// Whether a layer surface with an `exclusive` keyboard is holding the
    /// keyboard, in which case clicking a window must not take it away.
    pub(crate) fn layer_has_exclusive_keyboard(&self) -> bool {
        self.layer_focus.as_ref().is_some_and(|layer| {
            layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
        })
    }

    fn layer_is_mapped(&self, layer: &LayerSurface) -> bool {
        self.space
            .outputs()
            .any(|output| layer_map_for_output(output).layers().any(|l| l == layer))
    }

    /// The layer surface with this namespace, on any screen.
    pub(crate) fn layer_with_namespace(&self, namespace: &str) -> Option<LayerSurface> {
        self.space.outputs().find_map(|output| {
            layer_map_for_output(output)
                .layers()
                .find(|layer| layer.namespace() == namespace)
                .cloned()
        })
    }

    /// The layer surface on one of `layers` under a point, with its position
    /// on the screen, topmost first.
    pub(crate) fn layer_under(
        &self,
        layers: &[Layer],
        pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) -> Option<(
        LayerSurface,
        smithay::utils::Point<i32, smithay::utils::Logical>,
    )> {
        let output = self.space.output_under(pos).next()?;
        let output_geo = self.space.output_geometry(output)?;
        let map = layer_map_for_output(output);
        let relative = pos - output_geo.loc.to_f64();
        layers.iter().find_map(|&layer| {
            let surface = map.layer_under(layer, relative)?;
            let geo = map.layer_geometry(surface)?;
            Some((surface.clone(), geo.loc + output_geo.loc))
        })
    }
}

use smithay::{
    delegate_xdg_shell,
    desktop::{
        PopupKind, PopupManager, Space, Window, find_popup_root_surface, get_popup_toplevel_coords,
    },
    reexports::wayland_server::protocol::{wl_seat, wl_surface::WlSurface},
    utils::Serial,
    wayland::{
        compositor::with_states,
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
        },
    },
};

use crate::state::Tessera;

impl XdgShellHandler for Tessera {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Tiled into the focused window's space, and focused so you can type straight away.
        // A window we were asked to place goes where the request said instead (§8).
        let window = Window::new_wayland_window(surface);
        let placement = self.take_placement_for(&window);
        self.assign_window_id(&window);
        self.map_window(window, placement);

        // `Placement::Workspace` maps here first, then moves.
        if let Some(index) = self.pending_workspace.take() {
            self.move_focused_to_workspace(index);
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel() == Some(&surface))
            .cloned()
        else {
            return;
        };
        // Its sibling reclaims the space, and focus moves to the nearest neighbour.
        self.forget_window_id(&window);
        self.unmap_window(&window);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!(?err, "failed to track popup");
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // Popup grabs (menus closing on outside clicks) are not implemented yet.
    }
}

delegate_xdg_shell!(Tessera);

/// Sends initial configures for new toplevels and popups. Call on every `wl_surface.commit`.
pub fn handle_commit(popups: &mut PopupManager, space: &Space<Window>, surface: &WlSurface) {
    if let Some(window) = space
        .elements()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
    {
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().map(|data| data.initial_configure_sent))
                .unwrap_or(true)
        });
        if !initial_configure_sent && let Some(toplevel) = window.toplevel() {
            toplevel.send_configure();
        }
    }

    popups.commit(surface);
    if let Some(PopupKind::Xdg(popup)) = popups.find_popup(surface)
        && !popup.is_initial_configure_sent()
    {
        // The initial configure is always allowed, so this cannot fail.
        let _ = popup.send_configure();
    }
}

impl Tessera {
    /// Keeps popups (menus, tooltips) inside the output.
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self.window_for_surface(&root) else {
            return;
        };
        let Some(output_geo) = self
            .space
            .outputs()
            .next()
            .and_then(|output| self.space.output_geometry(output))
        else {
            return;
        };
        let Some(window_geo) = self.space.element_geometry(&window) else {
            return;
        };

        // The positioner target is relative to the popup's parent geometry.
        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

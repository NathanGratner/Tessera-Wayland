//! xdg-activation: how a spawned program proves which window is its own.

use smithay::{
    delegate_xdg_activation,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::xdg_activation::{
        XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
    },
};

use crate::state::Tessera;

impl XdgActivationHandler for Tessera {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Remember which surface presented which token; the placement code
        // reads this when the window is mapped.
        self.presented_tokens
            .insert(surface.clone(), token.to_string());

        // A client asking for attention for a window we already know: focus it.
        if let Some(window) = self.window_for_surface(&surface) {
            self.focus_anywhere(&window);
        }
    }
}

delegate_xdg_activation!(Tessera);

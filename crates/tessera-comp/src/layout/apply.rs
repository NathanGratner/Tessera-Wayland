//! Turns the split tree into real window geometry (design §3.3, "Apply").

use smithay::{
    desktop::Window, wayland::compositor::with_states, wayland::shell::xdg::XdgToplevelSurfaceData,
};

use super::tree::{Direction, Placement, Rect};
use crate::state::Tessera;

/// The launcher identifies itself with this app id (design §5).
pub const LAUNCHER_APP_ID: &str = "tessera.launcher";

/// Finds the launcher binary.
///
/// Prefers one sitting next to the compositor, so a freshly built `target/debug`
/// pair works without putting anything on `PATH`; otherwise the name is left
/// for the usual `PATH` lookup.
fn launcher_program(command: &str) -> String {
    let beside_us = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.join(command)))
        .filter(|path| path.is_file());

    match beside_us {
        Some(path) => path.to_string_lossy().into_owned(),
        None => command.to_string(),
    }
}

/// Reads a window's `xdg_toplevel` app id.
pub fn window_app_id(window: &Window) -> Option<String> {
    let surface = window.toplevel()?.wl_surface().clone();
    with_states(&surface, |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok())
            .and_then(|data| data.app_id.clone())
    })
}

/// Reads a window's `xdg_toplevel` title.
pub fn window_title(window: &Window) -> Option<String> {
    let surface = window.toplevel()?.wl_surface().clone();
    with_states(&surface, |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok())
            .and_then(|data| data.title.clone())
    })
}

impl Tessera {
    /// The usable area of the first output, in logical coordinates.
    pub fn output_area(&self) -> Rect {
        self.space
            .outputs()
            .next()
            .and_then(|output| self.space.output_geometry(output))
            .map(|geo| Rect::new(geo.loc.x, geo.loc.y, geo.size.w, geo.size.h))
            .unwrap_or_default()
    }

    /// Sizes and positions every window on the active workspace.
    pub fn apply_layout(&mut self) {
        let area = self.output_area();
        let workspace = &self.workspaces[self.active_workspace];
        let mut opts = self.layout_opts;
        if self.behaviour.smart_gaps && workspace.len() == 1 {
            opts.gaps = 0;
        }
        let placements = workspace.layout(area, opts);
        tracing::debug!(
            workspace = self.active_workspace + 1,
            tiles = placements.len(),
            rects = ?placements.iter().map(|(_, r)| (r.x, r.y, r.w, r.h)).collect::<Vec<_>>(),
            "layout",
        );

        for (window, rect) in placements {
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.size = Some((rect.w, rect.h).into());
                });
                toplevel.send_pending_configure();
            }
            // `false`: tiling never changes which window is activated.
            self.space.map_element(window, (rect.x, rect.y), false);
        }

        // The nested backend redraws every frame; on real hardware nothing
        // draws until something asks, so ask.
        self.render_all();
    }

    /// Adds a new window to the active workspace and focuses it.
    pub fn map_window(&mut self, window: Window, placement: Placement<Window>) {
        let area = self.output_area();
        let focus = self.focus.clone();
        self.workspaces[self.active_workspace].insert(
            window.clone(),
            placement,
            focus.as_ref(),
            area,
        );
        self.apply_layout();
        // With "new windows take focus" off, still focus a window when nothing
        // has focus, or the keyboard would go nowhere.
        if self.behaviour.focus_new_windows || self.focus.is_none() {
            self.focus_window(Some(&window));
        }
    }

    /// Removes a window from wherever it is and focuses its nearest neighbour.
    pub fn unmap_window(&mut self, window: &Window) {
        let area = self.output_area();
        let opts = self.layout_opts;
        let active = self.active_workspace;

        let mut next = None;
        for (index, workspace) in self.workspaces.iter_mut().enumerate() {
            if !workspace.contains(window) {
                continue;
            }
            if index == active {
                next = [
                    Direction::Right,
                    Direction::Left,
                    Direction::Down,
                    Direction::Up,
                ]
                .into_iter()
                .find_map(|dir| workspace.neighbor(window, dir, area, opts));
            }
            workspace.remove(window);
        }
        self.space.unmap_elem(window);
        self.apply_layout();

        if self.focus.as_ref() == Some(window) {
            let next = next.or_else(|| self.workspaces[active].ids().first().cloned());
            self.focus_window(next.as_ref());
        }
    }

    /// Moves keyboard focus to the neighbouring window.
    pub fn focus_direction(&mut self, dir: Direction) {
        let (Some(current), area, opts) =
            (self.focus.clone(), self.output_area(), self.layout_opts)
        else {
            return;
        };
        if let Some(next) =
            self.workspaces[self.active_workspace].neighbor(&current, dir, area, opts)
        {
            self.focus_window(Some(&next));
        }
    }

    /// Swaps the focused window with its neighbour.
    pub fn swap_direction(&mut self, dir: Direction) {
        let (Some(current), area, opts) =
            (self.focus.clone(), self.output_area(), self.layout_opts)
        else {
            return;
        };
        if self.workspaces[self.active_workspace].swap(&current, dir, area, opts) {
            self.apply_layout();
        }
    }

    /// Moves the edge nearest the focused window.
    pub fn resize_direction(&mut self, dir: Direction, step: f32) {
        let Some(current) = self.focus.clone() else {
            return;
        };
        if self.workspaces[self.active_workspace].resize(&current, dir, step) {
            self.apply_layout();
        }
    }

    /// Shows another workspace, hiding the current one's windows.
    pub fn switch_workspace(&mut self, index: usize) {
        if index >= self.workspaces.len() || index == self.active_workspace {
            return;
        }
        for window in self.workspaces[self.active_workspace].ids() {
            self.space.unmap_elem(&window);
        }
        self.active_workspace = index;
        self.apply_layout();

        let next = self.workspaces[index].ids().first().cloned();
        self.focus_window(next.as_ref());
        tracing::debug!(
            workspace = index + 1,
            windows = self.workspaces[index].len(),
            "workspace"
        );
    }

    /// Sends the focused window to another workspace and stays put.
    pub fn move_focused_to_workspace(&mut self, index: usize) {
        let Some(window) = self.focus.clone() else {
            return;
        };
        if index >= self.workspaces.len() || index == self.active_workspace {
            return;
        }
        let area = self.output_area();

        self.unmap_window(&window);
        self.workspaces[index].insert(window, Placement::Auto, None, area);
        tracing::debug!(workspace = index + 1, "moved window to workspace");
    }

    /// Focuses the launcher if it is open on this workspace, otherwise starts it.
    pub fn toggle_launcher(&mut self, command: &str) {
        match self.launcher_window() {
            Some(window) => self.focus_window(Some(&window)),
            None => {
                let program = launcher_program(command);
                let kind = self.launcher_placement();
                if let Err(err) = self.spawn_with(std::slice::from_ref(&program), kind) {
                    tracing::warn!(error = %format!("{err:#}"), program, "could not start the launcher");
                }
            }
        }
    }

    /// The launcher's window, if it is open on the active workspace.
    pub(crate) fn launcher_window(&self) -> Option<Window> {
        self.workspaces[self.active_workspace]
            .ids()
            .into_iter()
            .find(|window| window_app_id(window).as_deref() == Some(LAUNCHER_APP_ID))
    }

    /// Asks the focused window to close.
    pub fn close_focused(&mut self) {
        if let Some(toplevel) = self.focus.as_ref().and_then(|window| window.toplevel()) {
            toplevel.send_close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::launcher_program;

    #[test]
    fn an_unknown_launcher_name_is_left_for_the_path_lookup() {
        assert_eq!(
            launcher_program("definitely-not-installed-xyz"),
            "definitely-not-installed-xyz"
        );
    }

    #[test]
    fn a_binary_next_to_the_compositor_is_preferred() {
        // The test binary lives in target/debug/deps, so use a file that is
        // actually beside it: the test binary itself.
        let exe = std::env::current_exe().unwrap();
        let name = exe.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(launcher_program(&name), exe.to_string_lossy());
    }
}

//! Placing windows that were asked for over IPC (design §8).
//!
//! A spawn and the window it eventually produces are two separate events, and
//! Wayland offers no link between them. The compositor records what it asked
//! for, then matches the window when it appears.

use std::time::Instant;

use smithay::{desktop::Window, reexports::wayland_server::Resource};
use tessera_ipc::{Placement, Side, WindowId};

use super::tree::{Direction, Placement as TreePlacement};
use crate::{
    ipc::{
        PLACEMENT_TIMEOUT, PendingKind, PendingMeta, PendingPlacement, is_descendant, match_pending,
    },
    state::Tessera,
};

impl From<Side> for Direction {
    fn from(side: Side) -> Self {
        match side {
            Side::Left => Direction::Left,
            Side::Right => Direction::Right,
            Side::Above => Direction::Up,
            Side::Below => Direction::Down,
        }
    }
}

impl Tessera {
    /// Starts a program and remembers where its window should go.
    pub(crate) fn spawn_placed(
        &mut self,
        argv: &[String],
        placement: Placement,
        caller_pid: Option<u32>,
    ) -> anyhow::Result<u32> {
        let kind = self.resolve_placement(placement, caller_pid);
        self.spawn_with(argv, kind)
    }

    /// Starts a program whose window should go where `kind` says.
    pub(crate) fn spawn_with(&mut self, argv: &[String], kind: PendingKind) -> anyhow::Result<u32> {
        // Clients that honour XDG_ACTIVATION_TOKEN give us an exact match.
        let token = self
            .xdg_activation_state
            .create_external_token(None)
            .0
            .to_string();

        let pid = self
            .spawn_process(argv, Some(&token))
            .map_err(|err| anyhow::anyhow!("could not run `{}`: {err}", argv.join(" ")))?;

        self.pending_placements.push(PendingPlacement {
            pid,
            token: Some(token),
            kind,
            deadline: Instant::now() + PLACEMENT_TIMEOUT,
        });
        tracing::info!(pid, argv = ?argv, "spawned");
        Ok(pid)
    }

    /// Where a terminal opened without a caller goes: beside the launcher at
    /// its configured share when the launcher is open on this workspace, the
    /// same as its Terminal entry; otherwise tiled the usual way.
    ///
    /// Used by Mod+Return and by terminal-mode scripts started from a key, so
    /// every terminal opened while the launcher is up lands in the same place.
    pub(crate) fn beside_launcher_or_auto(&self) -> PendingKind {
        match self.launcher_window() {
            Some(anchor) => PendingKind::Beside {
                anchor,
                side: self.terminal_side().into(),
                ratio: self.beside_share(),
            },
            None => PendingKind::Auto,
        }
    }

    /// Where the launcher goes when it opens: on its configured side of the
    /// focused window, taking what the terminal share leaves, so opening the
    /// launcher next to a terminal gives the same split as opening a terminal
    /// next to the launcher. With nothing focused it tiles the usual way.
    pub(crate) fn launcher_placement(&self) -> PendingKind {
        let focused = self
            .focus
            .clone()
            .filter(|window| self.workspaces[self.active_workspace].contains(window));
        match focused {
            Some(anchor) => PendingKind::Beside {
                anchor,
                side: self.launcher_side().into(),
                ratio: 1.0 - self.beside_share(),
            },
            None => PendingKind::Auto,
        }
    }

    /// The `launcher.side` setting: which side of the focused window the launcher opens on.
    pub(crate) fn launcher_side(&self) -> Side {
        Side::from_name(&self.settings.text("launcher.side")).unwrap_or(Side::Left)
    }

    /// Terminals beside the launcher go on its other side, keeping it at its edge.
    pub(crate) fn terminal_side(&self) -> Side {
        self.launcher_side().opposite()
    }

    /// The `launcher.beside_share` setting as a fraction: a terminal's share of the launcher's tile.
    pub(crate) fn beside_share(&self) -> f32 {
        self.settings.int("launcher.beside_share") as f32 / 100.0
    }

    /// Turns an IPC placement into one the tiling code understands.
    fn resolve_placement(&self, placement: Placement, caller_pid: Option<u32>) -> PendingKind {
        match placement {
            Placement::Auto => PendingKind::Auto,
            Placement::Workspace { number } => {
                let index = (number.clamp(1, self.workspaces.len() as u8) - 1) as usize;
                PendingKind::Workspace(index)
            }
            Placement::BesideCaller { side, ratio } => {
                // The caller never names its own window: we find it from the
                // process on the other end of the socket.
                match caller_pid.and_then(|pid| self.window_of_process(pid)) {
                    Some(anchor) => PendingKind::Beside {
                        anchor,
                        side: side.into(),
                        ratio,
                    },
                    None => {
                        tracing::debug!(?caller_pid, "no window for the caller; tiling normally");
                        PendingKind::Auto
                    }
                }
            }
        }
    }

    /// The window belonging to a process, if it has one on the active workspace.
    ///
    /// A caller may be a child of the window it should anchor on: a script run
    /// from a terminal asks for a tile beside *that terminal*, not beside
    /// nothing. So an exact match is tried first, then ancestry.
    fn window_of_process(&self, pid: u32) -> Option<Window> {
        let windows = self.workspaces[self.active_workspace].ids();
        windows
            .iter()
            .find(|window| self.window_pid(window) == Some(pid))
            .or_else(|| {
                windows.iter().find(|window| {
                    self.window_pid(window)
                        .is_some_and(|owner| is_descendant(pid, owner))
                })
            })
            .cloned()
    }

    /// The process id behind a window's Wayland client.
    pub(crate) fn window_pid(&self, window: &Window) -> Option<u32> {
        let surface = window.toplevel()?.wl_surface();
        let client = surface.client()?;
        client
            .get_credentials(&self.display_handle)
            .ok()
            .map(|credentials| credentials.pid as u32)
    }

    /// Picks the placement a newly mapped window was promised, if any.
    ///
    /// Also drops spawns that never produced a window.
    pub(crate) fn take_placement_for(&mut self, window: &Window) -> TreePlacement<Window> {
        let now = Instant::now();
        self.pending_placements
            .retain(|pending| pending.deadline > now);

        let pid = self.window_pid(window);
        let token = window
            .toplevel()
            .and_then(|toplevel| self.presented_tokens.remove(toplevel.wl_surface()));

        let metas: Vec<PendingMeta> = self
            .pending_placements
            .iter()
            .map(|pending| PendingMeta {
                pid: pending.pid,
                token: pending.token.clone(),
                deadline: pending.deadline,
            })
            .collect();

        let Some(index) = match_pending(&metas, pid, token.as_deref(), now, is_descendant) else {
            return TreePlacement::Auto;
        };
        let pending = self.pending_placements.remove(index);
        tracing::debug!(pid = pending.pid, kind = ?pending.kind, "matched a pending placement");

        match pending.kind {
            PendingKind::Auto => TreePlacement::Auto,
            PendingKind::Beside {
                anchor,
                side,
                ratio,
            } => {
                // The anchor may have closed while we waited.
                if self.workspaces[self.active_workspace].contains(&anchor) {
                    TreePlacement::Beside {
                        anchor,
                        side,
                        ratio,
                    }
                } else {
                    TreePlacement::Auto
                }
            }
            PendingKind::Workspace(index) => {
                // Handled by the caller: map it here, then move it.
                self.pending_workspace = Some(index);
                TreePlacement::Auto
            }
        }
    }

    /// Gives a window an id for the IPC window list.
    pub(crate) fn assign_window_id(&mut self, window: &Window) -> WindowId {
        if let Some(id) = self.id_of_window(window) {
            return id;
        }
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;
        self.window_ids.push((id, window.clone()));
        id
    }

    /// Forgets a window's id when it closes.
    pub(crate) fn forget_window_id(&mut self, window: &Window) {
        self.window_ids.retain(|(_, known)| known != window);
    }

    /// The id previously given to this window.
    pub(crate) fn id_of_window(&self, window: &Window) -> Option<WindowId> {
        self.window_ids
            .iter()
            .find(|(_, known)| known == window)
            .map(|(id, _)| *id)
    }

    /// The window with this id, on any workspace.
    pub(crate) fn window_by_id(&self, id: WindowId) -> Option<Window> {
        self.window_ids
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, window)| window.clone())
    }

    /// Focuses a window wherever it is, switching workspace if necessary.
    pub(crate) fn focus_anywhere(&mut self, window: &Window) {
        if let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.contains(window))
            && index != self.active_workspace
        {
            self.switch_workspace(index);
        }
        self.focus_window(Some(window));
    }
}

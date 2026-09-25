use std::collections::HashMap;

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{FilterResult, Keysym, ModifiersState, keysyms, xkb},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::SERIAL_COUNTER,
    wayland::shell::wlr_layer::Layer,
};
use tessera_config::{Binding, ConfigValues};

use crate::{
    layout::Direction,
    state::{ModKey, Tessera, WORKSPACE_COUNT},
};

/// How far one resize step moves a split edge (design §3.3).
const RESIZE_STEP: f32 = 0.05;
/// The launcher binary, looked for beside the compositor and then on PATH.
const LAUNCHER: &str = "tessera-launcher";

/// The bindings configurable in `[bindings]`, and what each does.
///
/// Movement (H/J/K/L with Shift or Ctrl) and workspaces (1–9) stay fixed:
/// they are systematic sets, not individual keys.
const CONFIGURABLE: [(&str, Action); 6] = [
    ("bindings.launcher", Action::ToggleLauncher),
    ("bindings.launcher_alt", Action::ToggleLauncher),
    ("bindings.apps", Action::ToggleAppsOverlay),
    ("bindings.terminal", Action::SpawnTerminal),
    ("bindings.close", Action::CloseWindow),
    ("bindings.quit", Action::Quit),
];

/// What a compositor binding does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    CloseWindow,
    SpawnTerminal,
    ToggleLauncher,
    /// Open or close the application overlay.
    ToggleAppsOverlay,
    Focus(Direction),
    Swap(Direction),
    Resize(Direction),
    Workspace(usize),
    MoveToWorkspace(usize),
    /// Run the script at this index in the compositor's script list.
    RunScript(usize),
    /// Switch to this virtual terminal (Ctrl+Alt+F1…F12, real sessions only).
    SwitchVt(i32),
}

impl Action {
    /// What the binding does, for explaining a clash.
    fn describe(self) -> String {
        match self {
            Action::Quit => "quit Tessera".into(),
            Action::CloseWindow => "close the focused window".into(),
            Action::SpawnTerminal => "open a terminal".into(),
            Action::ToggleLauncher => "open the launcher".into(),
            Action::ToggleAppsOverlay => "open the application overlay".into(),
            Action::Focus(_) => "move focus".into(),
            Action::Swap(_) => "swap windows".into(),
            Action::Resize(_) => "resize a split".into(),
            Action::Workspace(index) => format!("switch to workspace {}", index + 1),
            Action::MoveToWorkspace(index) => format!("move a window to workspace {}", index + 1),
            Action::RunScript(_) => "run another script".into(),
            Action::SwitchVt(vt) => format!("switch to virtual terminal {vt}"),
        }
    }
}

/// A script's `bind` header, as handed to [`Bindings::add_scripts`].
pub struct ScriptBinding<'a> {
    /// Position in the compositor's script list.
    pub index: usize,
    /// The script's name, for messages.
    pub name: &'a str,
    /// The binding as written, e.g. `Mod+Shift+N`.
    pub text: &'a str,
}

/// The modifier combination of a binding. `mod_key` is Alt or Super (see [`ModKey`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mods {
    pub mod_key: bool,
    pub shift: bool,
    pub ctrl: bool,
}

impl Mods {
    const fn new(mod_key: bool, shift: bool, ctrl: bool) -> Self {
        Self {
            mod_key,
            shift,
            ctrl,
        }
    }

    fn from_state(mod_key: ModKey, state: &ModifiersState) -> Self {
        Self::new(mod_key.is_held(state), state.shift, state.ctrl)
    }
}

/// Modifier + key to action.
pub struct Bindings(HashMap<(Mods, u32), Action>);

impl Default for Bindings {
    fn default() -> Self {
        Self::from_config(&ConfigValues::default()).expect("the default bindings are valid")
    }
}

impl Bindings {
    /// Builds the table: the fixed movement and workspace keys, plus the
    /// bindings from `[bindings]`. Fails if a configured key name is unknown.
    pub fn from_config(values: &ConfigValues) -> Result<Self, String> {
        let mut map = Self::fixed();
        for (key, action) in CONFIGURABLE {
            let text = values.text(key);
            let binding = Binding::parse(&text).map_err(|err| format!("{key}: {err}"))?;
            let sym = keysym_named(&binding.key).ok_or_else(|| {
                format!(
                    "{key}: `{}` is not a key name xkb knows (try Return, space, backslash, F1)",
                    binding.key
                )
            })?;
            map.insert((Mods::new(true, binding.shift, binding.ctrl), sym), action);
        }
        Ok(Self(map))
    }

    /// Adds the scripts' bindings and returns, per script left out, why.
    ///
    /// A script never takes a key from Tessera itself: a clash with a built-in
    /// or configured binding drops the script's. Two scripts claiming the same
    /// key are both dropped, because neither has a better claim.
    pub fn add_scripts(&mut self, scripts: &[ScriptBinding<'_>]) -> Vec<(usize, String)> {
        let mut problems = Vec::new();
        let mut claims: HashMap<(Mods, u32), Vec<&ScriptBinding<'_>>> = HashMap::new();

        for script in scripts {
            let parsed = Binding::parse(script.text).map_err(|err| err.to_string());
            let key = parsed.and_then(|binding| {
                keysym_named(&binding.key)
                    .map(|sym| (Mods::new(true, binding.shift, binding.ctrl), sym))
                    .ok_or_else(|| format!("`{}` is not a key name xkb knows", binding.key))
            });
            match key {
                Ok(key) => claims.entry(key).or_default().push(script),
                Err(reason) => problems.push((script.index, format!("bind: {reason}"))),
            }
        }

        for (key, claimants) in claims {
            if let Some(existing) = self.0.get(&key) {
                for script in claimants {
                    problems.push((
                        script.index,
                        format!(
                            "bind: {} already does \"{}\"; the script's binding is ignored",
                            script.text,
                            existing.describe()
                        ),
                    ));
                }
            } else if claimants.len() > 1 {
                for script in &claimants {
                    let others: Vec<&str> = claimants
                        .iter()
                        .filter(|other| other.index != script.index)
                        .map(|other| other.name)
                        .collect();
                    problems.push((
                        script.index,
                        format!(
                            "bind: {} is also claimed by {}; both are ignored",
                            script.text,
                            others.join(", ")
                        ),
                    ));
                }
            } else {
                self.0.insert(key, Action::RunScript(claimants[0].index));
            }
        }
        problems.sort();
        problems
    }

    fn fixed() -> HashMap<(Mods, u32), Action> {
        let mut map = HashMap::new();
        let m = Mods::new(true, false, false);
        let m_shift = Mods::new(true, true, false);
        let m_ctrl = Mods::new(true, false, true);

        let directions = [
            (keysyms::KEY_h, Direction::Left),
            (keysyms::KEY_j, Direction::Down),
            (keysyms::KEY_k, Direction::Up),
            (keysyms::KEY_l, Direction::Right),
        ];
        for (sym, dir) in directions {
            map.insert((m, sym), Action::Focus(dir));
            map.insert((m_shift, sym), Action::Swap(dir));
            map.insert((m_ctrl, sym), Action::Resize(dir));
        }

        for index in 0..WORKSPACE_COUNT {
            let sym = keysyms::KEY_1 + index as u32;
            map.insert((m, sym), Action::Workspace(index));
            map.insert((m_shift, sym), Action::MoveToWorkspace(index));
        }
        map
    }

    fn get(&self, mods: Mods, sym: Option<Keysym>) -> Option<Action> {
        if !mods.mod_key {
            return None;
        }
        self.0.get(&(mods, sym?.raw())).copied()
    }
}

/// The VT number of an F-key, for Ctrl+Alt+F1…F12.
fn function_key(sym: Keysym) -> Option<i32> {
    let raw = sym.raw();
    (keysyms::KEY_F1..=keysyms::KEY_F12)
        .contains(&raw)
        .then(|| (raw - keysyms::KEY_F1) as i32 + 1)
}

/// Looks a key up by its xkb name.
///
/// Single letters are matched lowercase, because bindings compare against the
/// unshifted symbol: `Mod+Shift+E` and `Mod+Shift+e` mean the same key.
fn keysym_named(name: &str) -> Option<u32> {
    let name = if name.chars().count() == 1 {
        name.to_lowercase()
    } else {
        name.to_string()
    };
    let exact = xkb::keysym_from_name(&name, xkb::KEYSYM_NO_FLAGS);
    let sym = if exact.raw() == 0 {
        xkb::keysym_from_name(&name, xkb::KEYSYM_CASE_INSENSITIVE)
    } else {
        exact
    };
    (sym.raw() != 0).then_some(sym.raw())
}

impl Tessera {
    /// Routes one backend input event: bindings first, then the focused client.
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => self.on_keyboard::<I>(event),
            InputEvent::PointerMotionAbsolute { event, .. } => self.on_pointer_motion::<I>(event),
            InputEvent::PointerMotion { event, .. } => self.on_pointer_relative::<I>(event),
            InputEvent::PointerButton { event, .. } => self.on_pointer_button::<I>(event),
            InputEvent::PointerAxis { event, .. } => self.on_pointer_axis::<I>(event),
            _ => {}
        }
    }

    fn on_keyboard<I: InputBackend>(&mut self, event: I::KeyboardKeyEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let time = Event::time_msec(&event);
        let pressed = event.state() == KeyState::Pressed;
        let mod_key = self.mod_key;

        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let action = keyboard.input::<Action, _>(
            self,
            event.key_code(),
            event.state(),
            serial,
            time,
            |state, mods, handle| {
                if !pressed {
                    return FilterResult::Forward;
                }
                // Use the unshifted, layout-independent symbol so Mod+Shift+E matches `e`.
                let sym = handle.raw_latin_sym_or_raw_current_sym();
                // Ctrl+Alt+F1..F12 belongs to the session, not to any binding
                // table: it is how you get back to a TTY when something is wrong.
                if mods.ctrl
                    && mods.alt
                    && let Some(vt) = sym.and_then(function_key)
                {
                    return FilterResult::Intercept(Action::SwitchVt(vt));
                }
                match state.bindings.get(Mods::from_state(mod_key, mods), sym) {
                    Some(action) => FilterResult::Intercept(action),
                    None => FilterResult::Forward,
                }
            },
        );

        if let Some(action) = action {
            self.run_action(action);
        }
    }

    /// Lets go of every key Tessera believes is held, as if each had been released.
    ///
    /// For when the keyboard is taken away without releases following: the
    /// nested window losing focus, or a switch to another VT. Clients see the
    /// releases for keys they saw pressed; bindings do not fire, because they
    /// only act on presses.
    pub(crate) fn release_all_keys(&mut self) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let held = keyboard.pressed_keys();
        if held.is_empty() {
            return;
        }
        tracing::debug!(
            keys = held.len(),
            "keyboard taken away; releasing held keys"
        );
        let time = self.start_time.elapsed().as_millis() as u32;
        for key in held {
            keyboard.input::<(), _>(
                self,
                key,
                KeyState::Released,
                SERIAL_COUNTER.next_serial(),
                time,
                |_, _, _| FilterResult::Forward,
            );
        }
    }

    fn run_action(&mut self, action: Action) {
        tracing::debug!(?action, "binding");
        match action {
            Action::Quit => {
                tracing::info!("quit requested with {:?}+Shift+E", self.mod_key);
                self.loop_signal.stop();
            }
            Action::CloseWindow => self.close_focused(),
            Action::SpawnTerminal => {
                let terminal = self.behaviour.terminal.clone();
                let kind = self.beside_launcher_or_auto();
                if let Err(err) = self.spawn_with(&terminal, kind) {
                    tracing::warn!(error = %format!("{err:#}"), "could not open a terminal");
                }
            }
            Action::ToggleLauncher => self.toggle_launcher(LAUNCHER),
            Action::ToggleAppsOverlay => self.toggle_apps_overlay(LAUNCHER),
            Action::Focus(dir) => self.focus_direction(dir),
            Action::Swap(dir) => self.swap_direction(dir),
            Action::Resize(dir) => self.resize_direction(dir, RESIZE_STEP),
            Action::Workspace(index) => self.switch_workspace(index),
            Action::MoveToWorkspace(index) => self.move_focused_to_workspace(index),
            Action::RunScript(index) => self.run_script_at(index),
            Action::SwitchVt(vt) => self.switch_vt(vt),
        }
    }

    /// libinput reports how far the mouse moved, not where it is, so the
    /// position is ours to keep. It is clamped to the screens, or the pointer
    /// could wander off into nothing.
    fn on_pointer_relative<I: InputBackend>(&mut self, event: I::PointerMotionEvent) {
        let delta = event.delta();
        let location = self.pointer_location + delta;
        self.pointer_location = self.clamp_to_outputs(location);
        let pos = self.pointer_location;

        if self.behaviour.focus_follows_mouse {
            self.focus_under_pointer(pos);
        }

        let serial = SERIAL_COUNTER.next_serial();
        let under = self.surface_under(pos);
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time: event.time_msec(),
            },
        );
        pointer.frame(self);
        // No redraw from here. A touchpad reports motion hundreds of times a
        // second, and drawing on each one kept the event loop so busy that
        // libinput complained the compositor was reading input too slowly
        // ("event processing lagging behind"). The vblank loop already redraws
        // at the screen's pace, and picks the pointer up with it.
    }

    /// Keeps a point inside the screens.
    fn clamp_to_outputs(
        &self,
        point: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) -> smithay::utils::Point<f64, smithay::utils::Logical> {
        let mut bounds: Option<(f64, f64, f64, f64)> = None;
        for output in self.space.outputs() {
            let Some(geometry) = self.space.output_geometry(output) else {
                continue;
            };
            let (x1, y1) = (geometry.loc.x as f64, geometry.loc.y as f64);
            let (x2, y2) = (x1 + geometry.size.w as f64, y1 + geometry.size.h as f64);
            bounds = Some(match bounds {
                None => (x1, y1, x2, y2),
                Some((bx1, by1, bx2, by2)) => (bx1.min(x1), by1.min(y1), bx2.max(x2), by2.max(y2)),
            });
        }
        match bounds {
            Some((x1, y1, x2, y2)) => (
                point.x.clamp(x1, (x2 - 1.0).max(x1)),
                point.y.clamp(y1, (y2 - 1.0).max(y1)),
            )
                .into(),
            None => point,
        }
    }

    fn on_pointer_motion<I: InputBackend>(&mut self, event: I::PointerMotionAbsoluteEvent) {
        let Some(output) = self.space.outputs().next() else {
            return;
        };
        let Some(output_geo) = self.space.output_geometry(output) else {
            return;
        };
        let pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();
        self.pointer_location = pos;

        if self.behaviour.focus_follows_mouse {
            self.focus_under_pointer(pos);
        }

        let serial = SERIAL_COUNTER.next_serial();
        let under = self.surface_under(pos);
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time: event.time_msec(),
            },
        );
        pointer.frame(self);
    }

    /// Focuses the window under the pointer, for "focus follows mouse".
    fn focus_under_pointer(&mut self, pos: smithay::utils::Point<f64, smithay::utils::Logical>) {
        // Passing over an overlay or a bar is not passing over the window
        // beneath it.
        if self
            .layer_under(&[Layer::Overlay, Layer::Top], pos)
            .is_some()
        {
            return;
        }
        let Some(window) = self
            .space
            .element_under(pos)
            .map(|(window, _)| window.clone())
        else {
            return;
        };
        if self.focus.as_ref() == Some(&window) {
            return;
        }
        if self.behaviour.follows_mouse_skips_launcher
            && crate::layout::apply::window_app_id(&window).as_deref()
                == Some(crate::layout::apply::LAUNCHER_APP_ID)
        {
            return;
        }
        self.focus_window(Some(&window));
    }

    fn on_pointer_button<I: InputBackend>(&mut self, event: I::PointerButtonEvent) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let button_state = event.state();

        // Click to focus: the window under the pointer, or nothing when clicking the background.
        if button_state == ButtonState::Pressed && !pointer.is_grabbed() {
            self.click_to_focus(pointer.current_location());
        }

        pointer.button(
            self,
            &ButtonEvent {
                button: event.button_code(),
                state: button_state,
                serial,
                time: event.time_msec(),
            },
        );
        pointer.frame(self);
    }

    /// Gives focus to what was clicked.
    ///
    /// A layer surface that takes a keyboard gets it. A click on a window
    /// focuses the window, but while an overlay holds the keyboard exclusively
    /// the keyboard stays with the overlay: the protocol promises it that.
    fn click_to_focus(&mut self, pos: smithay::utils::Point<f64, smithay::utils::Logical>) {
        let layer = self
            .layer_under(&[Layer::Overlay, Layer::Top], pos)
            .or_else(|| {
                self.space
                    .element_under(pos)
                    .is_none()
                    .then(|| self.layer_under(&[Layer::Bottom, Layer::Background], pos))
                    .flatten()
            });
        if let Some((layer, _)) = layer {
            self.focus_layer(&layer);
            return;
        }

        if !self.layer_has_exclusive_keyboard() {
            self.layer_focus = None;
        }
        let window = self
            .space
            .element_under(pos)
            .map(|(window, _)| window.clone());
        if window.as_ref() != self.focus.as_ref() {
            self.focus_window(window.as_ref());
        } else {
            self.update_keyboard_focus();
        }
    }

    fn on_pointer_axis<I: InputBackend>(&mut self, event: I::PointerAxisEvent) {
        let source = event.source();
        let mut frame = AxisFrame::new(event.time_msec()).source(source);

        for axis in [Axis::Horizontal, Axis::Vertical] {
            let discrete = event.amount_v120(axis);
            let amount = event
                .amount(axis)
                .unwrap_or_else(|| discrete.unwrap_or(0.0) * 15.0 / 120.0);
            if amount != 0.0 {
                frame = frame.value(axis, amount);
                if let Some(discrete) = discrete {
                    frame = frame.v120(axis, discrete as i32);
                }
            }
            if source == AxisSource::Finger && event.amount(axis) == Some(0.0) {
                frame = frame.stop(axis);
            }
        }

        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        pointer.axis(self, frame);
        pointer.frame(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(alt: bool, logo: bool, shift: bool, ctrl: bool) -> ModifiersState {
        ModifiersState {
            alt,
            logo,
            shift,
            ctrl,
            ..Default::default()
        }
    }

    fn action(mod_key: ModKey, state: ModifiersState, sym: u32) -> Option<Action> {
        Bindings::default().get(Mods::from_state(mod_key, &state), Some(Keysym::from(sym)))
    }

    #[test]
    fn mod_shift_e_quits_with_the_backend_mod_key() {
        let alt = mods(true, false, true, false);
        let super_ = mods(false, true, true, false);
        assert_eq!(action(ModKey::Alt, alt, keysyms::KEY_e), Some(Action::Quit));
        assert_eq!(
            action(ModKey::Super, super_, keysyms::KEY_e),
            Some(Action::Quit)
        );
    }

    #[test]
    fn near_misses_are_forwarded_to_clients() {
        // Wrong modifier for the backend, no modifier at all, or an unbound key.
        assert_eq!(
            action(ModKey::Alt, mods(false, true, true, false), keysyms::KEY_e),
            None
        );
        assert_eq!(
            action(
                ModKey::Alt,
                mods(false, false, false, false),
                keysyms::KEY_h
            ),
            None
        );
        assert_eq!(
            action(ModKey::Alt, mods(true, false, false, false), keysyms::KEY_z),
            None
        );
        assert_eq!(
            Bindings::default().get(Mods::new(true, false, false), None),
            None
        );
    }

    #[test]
    fn direction_keys_focus_swap_and_resize() {
        let plain = mods(true, false, false, false);
        let shift = mods(true, false, true, false);
        let ctrl = mods(true, false, false, true);
        assert_eq!(
            action(ModKey::Alt, plain, keysyms::KEY_h),
            Some(Action::Focus(Direction::Left))
        );
        assert_eq!(
            action(ModKey::Alt, shift, keysyms::KEY_j),
            Some(Action::Swap(Direction::Down))
        );
        assert_eq!(
            action(ModKey::Alt, ctrl, keysyms::KEY_l),
            Some(Action::Resize(Direction::Right))
        );
        assert_eq!(
            action(ModKey::Alt, plain, keysyms::KEY_k),
            Some(Action::Focus(Direction::Up))
        );
    }

    #[test]
    fn number_keys_switch_and_move_between_workspaces() {
        let plain = mods(true, false, false, false);
        let shift = mods(true, false, true, false);
        assert_eq!(
            action(ModKey::Alt, plain, keysyms::KEY_1),
            Some(Action::Workspace(0))
        );
        assert_eq!(
            action(ModKey::Alt, plain, keysyms::KEY_9),
            Some(Action::Workspace(8))
        );
        assert_eq!(
            action(ModKey::Alt, shift, keysyms::KEY_2),
            Some(Action::MoveToWorkspace(1))
        );
        // There is no workspace 0 key.
        assert_eq!(action(ModKey::Alt, plain, keysyms::KEY_0), None);
    }

    #[test]
    fn configured_bindings_replace_the_defaults() {
        let values =
            ConfigValues::parse("[bindings]\nterminal = \"Mod+t\"\nquit = \"Mod+Ctrl+Escape\"\n")
                .unwrap();
        let bindings = Bindings::from_config(&values).unwrap();
        let get = |mods: Mods, sym: u32| bindings.get(mods, Some(Keysym::from(sym)));

        assert_eq!(
            get(Mods::new(true, false, false), keysyms::KEY_t),
            Some(Action::SpawnTerminal)
        );
        assert_eq!(
            get(Mods::new(true, false, true), keysyms::KEY_Escape),
            Some(Action::Quit)
        );
        assert_eq!(
            get(Mods::new(true, false, false), keysyms::KEY_Return),
            None,
            "the old terminal binding is gone"
        );
        // Movement keys are not configurable and stay put.
        assert_eq!(
            get(Mods::new(true, false, false), keysyms::KEY_h),
            Some(Action::Focus(Direction::Left))
        );
    }

    #[test]
    fn an_unknown_key_name_is_refused_with_suggestions() {
        let values = ConfigValues::parse("[bindings]\nclose = \"Mod+Enterr\"\n").unwrap();
        let err = Bindings::from_config(&values).err().unwrap();
        assert!(err.contains("bindings.close"), "{err}");
        assert!(err.contains("Enterr"), "{err}");
        assert!(err.contains("Return"), "suggests real names: {err}");
    }

    #[test]
    fn single_letter_keys_ignore_case() {
        assert_eq!(keysym_named("E"), Some(keysyms::KEY_e));
        assert_eq!(keysym_named("e"), Some(keysyms::KEY_e));
        assert_eq!(keysym_named("Return"), Some(keysyms::KEY_Return));
        assert_eq!(keysym_named("return"), Some(keysyms::KEY_Return));
        assert_eq!(keysym_named("NotAKey"), None);
    }

    fn script(index: usize, name: &'static str, text: &'static str) -> ScriptBinding<'static> {
        ScriptBinding { index, name, text }
    }

    #[test]
    fn script_bindings_join_the_table() {
        let mut bindings = Bindings::default();
        let problems = bindings.add_scripts(&[script(0, "notes", "Mod+Shift+N")]);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            bindings.get(
                Mods::new(true, true, false),
                Some(Keysym::from(keysyms::KEY_n))
            ),
            Some(Action::RunScript(0))
        );
    }

    #[test]
    fn a_script_never_takes_a_key_from_tessera() {
        let mut bindings = Bindings::default();
        let problems = bindings.add_scripts(&[script(3, "sneaky", "Mod+Shift+E")]);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].0, 3);
        assert!(problems[0].1.contains("quit Tessera"), "{}", problems[0].1);
        assert_eq!(
            bindings.get(
                Mods::new(true, true, false),
                Some(Keysym::from(keysyms::KEY_e))
            ),
            Some(Action::Quit),
            "quit still works"
        );
    }

    #[test]
    fn two_scripts_on_one_key_are_both_left_out() {
        let mut bindings = Bindings::default();
        let problems = bindings.add_scripts(&[
            script(0, "first", "Mod+Shift+N"),
            script(1, "second", "Mod+shift+n"),
            script(2, "third", "Mod+Ctrl+N"),
        ]);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].1.contains("second"), "{}", problems[0].1);
        assert!(problems[1].1.contains("first"), "{}", problems[1].1);
        assert_eq!(
            bindings.get(
                Mods::new(true, true, false),
                Some(Keysym::from(keysyms::KEY_n))
            ),
            None
        );
        assert_eq!(
            bindings.get(
                Mods::new(true, false, true),
                Some(Keysym::from(keysyms::KEY_n))
            ),
            Some(Action::RunScript(2)),
            "an unrelated script keeps its key"
        );
    }

    #[test]
    fn an_unknown_script_key_is_explained() {
        let mut bindings = Bindings::default();
        let problems = bindings.add_scripts(&[script(0, "x", "Mod+Nope")]);
        assert!(problems[0].1.contains("Nope"), "{}", problems[0].1);
    }

    #[test]
    fn ctrl_alt_f_keys_switch_virtual_terminals() {
        assert_eq!(function_key(Keysym::from(keysyms::KEY_F1)), Some(1));
        assert_eq!(function_key(Keysym::from(keysyms::KEY_F7)), Some(7));
        assert_eq!(function_key(Keysym::from(keysyms::KEY_F12)), Some(12));
        assert_eq!(function_key(Keysym::from(keysyms::KEY_e)), None);
        assert_eq!(function_key(Keysym::from(keysyms::KEY_Escape)), None);
    }

    #[test]
    fn window_and_session_bindings() {
        let plain = mods(true, false, false, false);
        assert_eq!(
            action(ModKey::Alt, plain, keysyms::KEY_space),
            Some(Action::ToggleLauncher)
        );
        // Plasma keeps Mod+Space for itself, so backslash opens it too.
        assert_eq!(
            action(ModKey::Alt, plain, keysyms::KEY_backslash),
            Some(Action::ToggleLauncher)
        );
        assert_eq!(
            action(ModKey::Alt, plain, keysyms::KEY_q),
            Some(Action::CloseWindow)
        );
        assert_eq!(
            action(ModKey::Alt, plain, keysyms::KEY_Return),
            Some(Action::SpawnTerminal)
        );
    }
}

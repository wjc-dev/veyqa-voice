pub mod carbon;
pub mod gesture;
pub mod listener;
pub mod monitor;
pub mod tap;

use std::collections::HashSet;
use std::time::Duration;

use gesture::{GestureSink, HotkeyMode, ModifierGesture, TimerRequest};

pub const MASK_SHIFT: u64 = 0x0002_0000;
pub const MASK_CONTROL: u64 = 0x0004_0000;
pub const MASK_OPTION: u64 = 0x0008_0000;
pub const MASK_COMMAND: u64 = 0x0010_0000;
pub const MASK_FUNCTION: u64 = 0x0080_0000;

const ALL_MODIFIER_MASKS: u64 =
    MASK_SHIFT | MASK_CONTROL | MASK_OPTION | MASK_COMMAND | MASK_FUNCTION;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawEventType {
    FlagsChanged,
    KeyDown,
    KeyUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    Command,
    Shift,
    Option,
    Control,
    Function,
}

impl Modifier {
    fn mask(self) -> u64 {
        match self {
            Self::Command => MASK_COMMAND,
            Self::Shift => MASK_SHIFT,
            Self::Option => MASK_OPTION,
            Self::Control => MASK_CONTROL,
            Self::Function => MASK_FUNCTION,
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "command" => Some(Self::Command),
            "shift" => Some(Self::Shift),
            "option" => Some(Self::Option),
            "control" => Some(Self::Control),
            "function" => Some(Self::Function),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HotkeySpec {
    Modifier {
        vk: i64,
        modifier: Modifier,
    },
    Key {
        vk: i64,
        modifiers: Vec<Modifier>,
    },
}

impl HotkeySpec {
    pub fn vk(&self) -> i64 {
        match self {
            Self::Modifier { vk, .. } | Self::Key { vk, .. } => *vk,
        }
    }

    pub fn mask(&self) -> u64 {
        match self {
            Self::Modifier { modifier, .. } => modifier.mask(),
            Self::Key { modifiers, .. } => modifiers.iter().fold(0, |mask, item| mask | item.mask()),
        }
    }

    pub fn is_modifier(&self) -> bool {
        matches!(self, Self::Modifier { .. })
    }

    pub fn hold_delay(&self) -> Duration {
        // Right Command is the parallel-install hotkey; keep it at the same
        // guard as right Option so the two builds are directly comparable.
        if matches!(self.vk(), 61 | 54) {
            ModifierGesture::RIGHT_OPTION_HOLD_DELAY
        } else {
            ModifierGesture::DEFAULT_HOLD_DELAY
        }
    }
}

pub fn parse_hotkey(value: &str) -> Option<HotkeySpec> {
    let special = match value {
        "fn" => Some((63, Modifier::Function)),
        "left_option" => Some((58, Modifier::Option)),
        "right_option" => Some((61, Modifier::Option)),
        "left_command" => Some((55, Modifier::Command)),
        "right_command" => Some((54, Modifier::Command)),
        "left_control" => Some((59, Modifier::Control)),
        "right_control" => Some((62, Modifier::Control)),
        _ => None,
    };
    if let Some((vk, modifier)) = special {
        return Some(HotkeySpec::Modifier { vk, modifier });
    }
    if !value.starts_with("keycode:") {
        return None;
    }

    let mut keycode = None;
    let mut mods = None;
    let mut seen = HashSet::new();
    for part in value.split(';') {
        let (key, raw) = part.split_once(':')?;
        if !seen.insert(key) {
            return None;
        }
        match key {
            "keycode" => keycode = Some(raw.parse::<i64>().ok()?),
            "mods" => mods = Some(raw),
            _ => return None,
        }
    }
    let vk = keycode?;
    let raw_mods = mods?;
    if !(0..=127).contains(&vk) {
        return None;
    }

    let names: Vec<&str> = raw_mods.split('+').filter(|name| !name.is_empty()).collect();
    if names.is_empty() {
        return None;
    }
    let mut unique = HashSet::new();
    if !names.iter().all(|name| unique.insert(*name)) {
        return None;
    }
    let modifiers: Option<Vec<Modifier>> = names.iter().map(|name| Modifier::parse(name)).collect();
    let modifiers = modifiers?;

    let normalized = format!(
        "keycode:{vk};mods:{}",
        modifiers
            .iter()
            .map(|modifier| match modifier {
                Modifier::Command => "command",
                Modifier::Shift => "shift",
                Modifier::Option => "option",
                Modifier::Control => "control",
                Modifier::Function => "function",
            })
            .collect::<Vec<_>>()
            .join("+")
    );
    if normalized != value {
        return None;
    }

    Some(HotkeySpec::Key { vk, modifiers })
}

pub const ESCAPE_KEYCODE: i64 = 53;

pub const VALID_REGISTERED_MODIFIERS: [&str; 5] =
    ["command", "shift", "option", "control", "function"];

/// Resolve stale or hand-edited backend values to a usable listener.
pub fn backend_for_hotkey(spec: Option<&HotkeySpec>, requested: &str) -> &'static str {
    if requested == "off" {
        return "off";
    }
    match spec {
        Some(HotkeySpec::Key { .. }) => "registered",
        _ => "passive",
    }
}

pub fn display_label(configured: &str, override_label: &str) -> String {
    match configured {
        "fn" => "Fn / Globe".to_string(),
        "left_option" => "左 Option".to_string(),
        "right_option" => "右 Option".to_string(),
        "left_command" => "左 Command".to_string(),
        "right_command" => "右 Command".to_string(),
        "left_control" => "左 Control".to_string(),
        "right_control" => "右 Control".to_string(),
        _ => {
            if override_label.is_empty() {
                "自定义快捷键".to_string()
            } else {
                override_label.to_string()
            }
        }
    }
}

/// Keys that must not be observed while the settings app is recording a shortcut.
pub fn suppression(spec: Option<&HotkeySpec>) -> (HashSet<i64>, u64) {
    match spec {
        Some(spec) => {
            let mut keys = HashSet::new();
            keys.insert(spec.vk());
            (keys, spec.mask())
        }
        None => (HashSet::new(), 0),
    }
}

/// The capture lock file holds the settings helper's pid.
pub fn shortcut_capture_pid(text: &str) -> Option<i32> {
    text.trim().parse::<i32>().ok()
}

pub fn modifier_is_down(spec: &HotkeySpec, flags: u64) -> bool {
    let (side, pair) = match spec.vk() {
        58 => (0x20, 0x60),
        61 => (0x40, 0x60),
        55 => (0x08, 0x18),
        54 => (0x10, 0x18),
        59 => (0x01, 0x2001),
        62 => (0x2000, 0x2001),
        _ => (0, 0),
    };
    if flags & pair != 0 {
        flags & side != 0
    } else {
        flags & spec.mask() != 0
    }
}

fn opposite_side_mask(vk: i64) -> u64 {
    match vk {
        58 => 0x40,
        61 => 0x20,
        55 => 0x10,
        54 => 0x08,
        59 => 0x2000,
        62 => 0x01,
        _ => 0,
    }
}

pub struct HotkeyHandler {
    spec: Option<HotkeySpec>,
    mode: HotkeyMode,
    gesture: ModifierGesture,
    other_keys_down: HashSet<i64>,
    pressed: bool,
    missing_release_polls: u8,
}

impl HotkeyHandler {
    pub fn new(spec: Option<HotkeySpec>, mode: HotkeyMode) -> Self {
        let hold_delay = spec
            .as_ref()
            .map(HotkeySpec::hold_delay)
            .unwrap_or(ModifierGesture::DEFAULT_HOLD_DELAY);
        Self {
            spec,
            mode,
            gesture: ModifierGesture::new(mode, hold_delay),
            other_keys_down: HashSet::new(),
            pressed: false,
            missing_release_polls: 0,
        }
    }

    pub fn spec(&self) -> Option<&HotkeySpec> {
        self.spec.as_ref()
    }

    pub fn pressed(&self) -> bool {
        self.pressed
    }

    pub fn reconfigure<S: GestureSink>(
        &mut self,
        spec: Option<HotkeySpec>,
        mode: HotkeyMode,
        sink: &mut S,
    ) {
        self.gesture.reset(sink);
        let hold_delay = spec
            .as_ref()
            .map(HotkeySpec::hold_delay)
            .unwrap_or(ModifierGesture::DEFAULT_HOLD_DELAY);
        self.gesture = ModifierGesture::new(mode, hold_delay);
        self.spec = spec;
        self.mode = mode;
        self.other_keys_down.clear();
        self.pressed = false;
        self.missing_release_polls = 0;
    }

    /// Returns whether the event was consumed by the configured shortcut and,
    /// for a hold gesture, the timer the caller must schedule.
    pub fn handle<S, F>(
        &mut self,
        event: RawEventType,
        vk: i64,
        flags: u64,
        sink: &mut S,
        mut key_state: F,
    ) -> (bool, Option<TimerRequest>)
    where
        S: GestureSink,
        F: FnMut(i64) -> bool,
    {
        let Some(spec) = self.spec.clone() else {
            return (false, None);
        };

        if spec.is_modifier() {
            if event == RawEventType::KeyDown {
                self.other_keys_down.insert(vk);
                self.gesture.chord(sink);
                return (false, None);
            }
            if event == RawEventType::KeyUp {
                self.other_keys_down.remove(&vk);
                return (false, None);
            }
            if event != RawEventType::FlagsChanged {
                return (false, None);
            }
            if vk != spec.vk() {
                self.gesture.chord(sink);
                return (false, None);
            }

            let pressed = modifier_is_down(&spec, flags);
            self.pressed = pressed;
            self.missing_release_polls = 0;
            if pressed {
                if !self.other_keys_down.is_empty() {
                    self.other_keys_down.retain(|code| key_state(*code));
                }
                let blocked = !self.other_keys_down.is_empty()
                    || (flags & ALL_MODIFIER_MASKS) != spec.mask()
                    || flags & opposite_side_mask(spec.vk()) != 0;
                let timer = self.gesture.press(blocked);
                (true, timer)
            } else {
                self.gesture.release(sink, true);
                (true, None)
            }
        } else {
            if event == RawEventType::FlagsChanged
                && self.pressed
                && (flags & spec.mask()) != spec.mask()
            {
                self.set_pressed(sink, false);
                return (false, None);
            }
            if vk != spec.vk() || event == RawEventType::FlagsChanged {
                return (false, None);
            }
            match event {
                RawEventType::KeyDown => {
                    if flags & spec.mask() == spec.mask() {
                        self.set_pressed(sink, true);
                        (true, None)
                    } else {
                        (false, None)
                    }
                }
                RawEventType::KeyUp => {
                    self.set_pressed(sink, false);
                    (true, None)
                }
                RawEventType::FlagsChanged => (false, None),
            }
        }
    }

    pub fn on_timer<S: GestureSink>(&mut self, generation: u64, sink: &mut S) -> bool {
        if !self.spec.as_ref().is_some_and(HotkeySpec::is_modifier) {
            return false;
        }
        self.gesture.on_timer(generation, sink)
    }

    /// Lost-event fallback: confirm a missing release on two consecutive polls.
    /// `key_down` and `flags` come from `CGEventSourceKeyState`/`FlagsState`.
    pub fn reconcile_release<S: GestureSink + ?Sized>(
        &mut self,
        sink: &mut S,
        key_down: bool,
        flags: u64,
    ) {
        if !self.pressed {
            return;
        }
        let Some(spec) = self.spec.clone() else {
            return;
        };
        let modifier_down = spec.is_modifier() && modifier_is_down(&spec, flags);
        if key_down || modifier_down {
            self.missing_release_polls = 0;
            return;
        }
        self.missing_release_polls = self.missing_release_polls.saturating_add(1);
        if self.missing_release_polls < 2 {
            return;
        }
        if spec.is_modifier() {
            self.pressed = false;
            self.gesture.release(sink, false);
        } else {
            self.set_pressed(sink, false);
        }
    }

    pub fn recover<S: GestureSink>(&mut self, sink: &mut S) {
        self.pressed = false;
        self.missing_release_polls = 0;
        self.other_keys_down.clear();
        if self.spec.as_ref().is_some_and(HotkeySpec::is_modifier) {
            self.gesture.reset(sink);
        } else if self.mode == HotkeyMode::Hold {
            sink.cancel();
        }
    }

    /// Carbon only reports the registered chord; forget a press without
    /// stopping or toggling anything (settings is recording a new shortcut).
    pub fn clear_pressed(&mut self) {
        self.pressed = false;
        self.missing_release_polls = 0;
    }

    fn set_pressed<S: GestureSink + ?Sized>(&mut self, sink: &mut S, pressed: bool) {
        if pressed == self.pressed {
            return;
        }
        self.pressed = pressed;
        self.missing_release_polls = 0;
        match (self.mode, pressed) {
            (HotkeyMode::Hold, true) => {
                sink.start();
            }
            (HotkeyMode::Toggle, true) => sink.toggle(),
            (HotkeyMode::Hold, false) => sink.stop(),
            (HotkeyMode::Toggle, false) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::gesture::{GestureSink, TimerRequest};
    use super::*;

    struct TestSink {
        actions: Vec<&'static str>,
        recording: bool,
        deny_start: bool,
    }

    impl TestSink {
        fn new() -> Self {
            Self {
                actions: Vec::new(),
                recording: false,
                deny_start: false,
            }
        }

        fn actions(&self) -> &[&'static str] {
            &self.actions
        }
    }

    impl GestureSink for TestSink {
        fn start(&mut self) -> bool {
            if self.deny_start {
                return false;
            }
            self.actions.push("start");
            self.recording = true;
            true
        }

        fn stop(&mut self) {
            self.actions.push("stop");
            self.recording = false;
        }

        fn cancel(&mut self) {
            self.actions.push("cancel");
            self.recording = false;
        }

        fn toggle(&mut self) {
            self.actions.push("toggle");
            self.recording = !self.recording;
        }
    }

    fn command_left() -> HotkeySpec {
        HotkeySpec::Modifier {
            vk: 55,
            modifier: Modifier::Command,
        }
    }

    fn fire<S: GestureSink>(handler: &mut HotkeyHandler, timer: TimerRequest, sink: &mut S) {
        assert!(handler.on_timer(timer.generation, sink));
    }

    #[test]
    fn parses_special_modifiers() {
        assert_eq!(
            parse_hotkey("right_option"),
            Some(HotkeySpec::Modifier {
                vk: 61,
                modifier: Modifier::Option
            })
        );
        assert_eq!(
            parse_hotkey("fn"),
            Some(HotkeySpec::Modifier {
                vk: 63,
                modifier: Modifier::Function
            })
        );
        assert_eq!(parse_hotkey("right_shift"), None);
        assert_eq!(parse_hotkey("Right_Option"), None);
    }

    #[test]
    fn parses_keycode_hotkeys_strictly() {
        let spec = parse_hotkey("keycode:49;mods:control+option").expect("valid");
        assert_eq!(spec.vk(), 49);
        assert_eq!(spec.mask(), MASK_CONTROL | MASK_OPTION);
        assert_eq!(
            parse_hotkey("keycode:49;mods:option+control")
                .expect("order preserved")
                .mask(),
            MASK_CONTROL | MASK_OPTION
        );
        assert_eq!(parse_hotkey("keycode:49;mods:"), None);
        assert_eq!(parse_hotkey("keycode:49;mods:command+command"), None);
        assert_eq!(parse_hotkey("keycode:200;mods:command"), None);
        assert_eq!(parse_hotkey("mods:command;keycode:49"), None);
        assert_eq!(parse_hotkey("keycode:49;mods:hyper"), None);
    }

    #[test]
    fn detects_left_and_right_sides() {
        let mut spec = command_left();
        assert!(modifier_is_down(&spec, MASK_COMMAND));
        assert!(modifier_is_down(&spec, 0x08));
        assert!(!modifier_is_down(&spec, 0x10));
        spec = HotkeySpec::Modifier {
            vk: 61,
            modifier: Modifier::Option,
        };
        assert!(modifier_is_down(&spec, MASK_OPTION | 0x40));
        assert!(!modifier_is_down(&spec, MASK_OPTION | 0x20));
    }

    #[test]
    fn voice_modifiers_have_short_guard() {
        for name in ["right_option", "right_command"] {
            let spec = parse_hotkey(name).unwrap();
            assert_eq!(spec.hold_delay(), Duration::from_millis(80));
        }
        let spec = command_left();
        assert_eq!(spec.hold_delay(), Duration::from_millis(180));
    }

    #[test]
    fn command_shortcuts_never_toggle() {
        for release_modifier_first in [false, true] {
            let mut handler = HotkeyHandler::new(Some(command_left()), HotkeyMode::Toggle);
            let mut sink = TestSink::new();
            let none = |_: i64| false;

            handler.handle(
                RawEventType::FlagsChanged,
                55,
                MASK_COMMAND | 0x08,
                &mut sink,
                none,
            );
            handler.handle(RawEventType::KeyDown, 0, MASK_COMMAND, &mut sink, none);
            if release_modifier_first {
                handler.handle(RawEventType::FlagsChanged, 55, 0, &mut sink, none);
            }
            handler.handle(RawEventType::KeyUp, 0, 0, &mut sink, none);
            if !release_modifier_first {
                handler.handle(RawEventType::FlagsChanged, 55, 0, &mut sink, none);
            }
            assert!(sink.actions().is_empty());

            handler.handle(
                RawEventType::FlagsChanged,
                55,
                MASK_COMMAND | 0x08,
                &mut sink,
                none,
            );
            handler.handle(RawEventType::FlagsChanged, 55, 0, &mut sink, none);
            assert_eq!(sink.actions(), &["toggle"]);
        }
    }

    #[test]
    fn chord_does_not_stop_an_existing_toggle_recording() {
        let mut handler = HotkeyHandler::new(Some(command_left()), HotkeyMode::Toggle);
        let mut sink = TestSink::new();
        sink.recording = true;
        let none = |_: i64| false;
        handler.handle(
            RawEventType::FlagsChanged,
            55,
            MASK_COMMAND | 0x08,
            &mut sink,
            none,
        );
        handler.handle(RawEventType::KeyDown, 0, MASK_COMMAND, &mut sink, none);
        handler.handle(RawEventType::FlagsChanged, 55, 0, &mut sink, none);
        assert!(sink.recording);
        assert!(sink.actions().is_empty());
    }

    #[test]
    fn keys_already_down_and_extra_modifiers_do_not_trigger() {
        let mut handler = HotkeyHandler::new(Some(command_left()), HotkeyMode::Toggle);
        let mut sink = TestSink::new();
        let none = |_: i64| false;
        handler.handle(RawEventType::KeyDown, 0, 0, &mut sink, none);
        handler.handle(
            RawEventType::FlagsChanged,
            55,
            MASK_COMMAND | 0x08,
            &mut sink,
            |code| code == 0,
        );
        handler.handle(RawEventType::FlagsChanged, 55, 0, &mut sink, none);
        handler.handle(RawEventType::KeyUp, 0, 0, &mut sink, none);
        handler.handle(
            RawEventType::FlagsChanged,
            55,
            MASK_COMMAND | MASK_SHIFT | 0x08,
            &mut sink,
            |_| false,
        );
        handler.handle(RawEventType::FlagsChanged, 55, 0, &mut sink, none);
        assert!(sink.actions().is_empty());
    }

    #[test]
    fn hold_chord_before_threshold_never_opens_microphone() {
        let mut handler = HotkeyHandler::new(Some(command_left()), HotkeyMode::Hold);
        let mut sink = TestSink::new();
        let none = |_: i64| false;
        let (_, timer) = handler.handle(
            RawEventType::FlagsChanged,
            55,
            MASK_COMMAND | 0x08,
            &mut sink,
            none,
        );
        let timer = timer.expect("timer");
        assert!(sink.actions().is_empty());
        handler.handle(RawEventType::KeyDown, 0, MASK_COMMAND, &mut sink, none);
        assert!(!handler.on_timer(timer.generation, &mut sink));
        handler.handle(RawEventType::FlagsChanged, 55, 0, &mut sink, none);
        assert!(sink.actions().is_empty());
    }

    #[test]
    fn late_chord_cancels_tentative_hold_without_committing_text() {
        let mut handler = HotkeyHandler::new(Some(command_left()), HotkeyMode::Hold);
        let mut sink = TestSink::new();
        let none = |_: i64| false;
        let (_, timer) = handler.handle(
            RawEventType::FlagsChanged,
            55,
            MASK_COMMAND | 0x08,
            &mut sink,
            none,
        );
        fire(&mut handler, timer.unwrap(), &mut sink);
        assert_eq!(sink.actions(), &["start"]);
        handler.handle(RawEventType::KeyDown, 0, MASK_COMMAND, &mut sink, none);
        handler.handle(RawEventType::FlagsChanged, 55, 0, &mut sink, none);
        assert_eq!(sink.actions(), &["start", "cancel"]);
    }

    #[test]
    fn old_timer_cannot_trigger_a_later_gesture() {
        let mut handler = HotkeyHandler::new(Some(command_left()), HotkeyMode::Hold);
        let mut sink = TestSink::new();
        let none = |_: i64| false;
        let (_, old) = handler.handle(
            RawEventType::FlagsChanged,
            55,
            MASK_COMMAND | 0x08,
            &mut sink,
            none,
        );
        handler.handle(RawEventType::FlagsChanged, 55, 0, &mut sink, none);
        let (_, new) = handler.handle(
            RawEventType::FlagsChanged,
            55,
            MASK_COMMAND | 0x08,
            &mut sink,
            none,
        );
        assert!(!handler.on_timer(old.unwrap().generation, &mut sink));
        assert!(sink.actions().is_empty());
        fire(&mut handler, new.unwrap(), &mut sink);
        handler.handle(RawEventType::FlagsChanged, 55, 0, &mut sink, none);
        assert_eq!(sink.actions(), &["start", "stop"]);
    }

    #[test]
    fn capture_start_before_threshold_prevents_recording() {
        let mut handler = HotkeyHandler::new(Some(command_left()), HotkeyMode::Hold);
        let mut sink = TestSink::new();
        sink.deny_start = true;
        let none = |_: i64| false;
        let (_, timer) = handler.handle(
            RawEventType::FlagsChanged,
            55,
            MASK_COMMAND | 0x08,
            &mut sink,
            none,
        );
        assert!(!handler.on_timer(timer.unwrap().generation, &mut sink));
        handler.handle(RawEventType::FlagsChanged, 55, 0, &mut sink, none);
        assert!(sink.actions().is_empty());
    }

    #[test]
    fn poll_release_does_not_invent_a_toggle_tap() {
        let mut handler = HotkeyHandler::new(Some(command_left()), HotkeyMode::Toggle);
        let mut sink = TestSink::new();
        let none = |_: i64| false;
        handler.handle(
            RawEventType::FlagsChanged,
            55,
            MASK_COMMAND | 0x08,
            &mut sink,
            none,
        );
        handler.reconcile_release(&mut sink, false, 0);
        handler.reconcile_release(&mut sink, false, 0);
        assert!(sink.actions().is_empty());
    }

    #[test]
    fn listener_recovery_cancels_pending_timer() {
        let mut handler = HotkeyHandler::new(Some(command_left()), HotkeyMode::Hold);
        let mut sink = TestSink::new();
        let none = |_: i64| false;
        let (_, timer) = handler.handle(
            RawEventType::FlagsChanged,
            55,
            MASK_COMMAND | 0x08,
            &mut sink,
            none,
        );
        handler.recover(&mut sink);
        assert!(!handler.on_timer(timer.unwrap().generation, &mut sink));
        assert!(sink.actions().is_empty());
    }

    #[test]
    fn opposite_command_and_shift_chords_do_not_toggle() {
        let mut handler = HotkeyHandler::new(Some(command_left()), HotkeyMode::Toggle);
        let mut sink = TestSink::new();
        let none = |_: i64| false;
        handler.handle(
            RawEventType::FlagsChanged,
            55,
            MASK_COMMAND | 0x10 | 0x08,
            &mut sink,
            none,
        );
        handler.handle(RawEventType::FlagsChanged, 55, 0x10, &mut sink, none);
        handler.handle(
            RawEventType::FlagsChanged,
            55,
            MASK_COMMAND | 0x08,
            &mut sink,
            none,
        );
        handler.handle(
            RawEventType::FlagsChanged,
            56,
            MASK_SHIFT | MASK_COMMAND,
            &mut sink,
            none,
        );
        handler.handle(RawEventType::FlagsChanged, 55, 0, &mut sink, none);
        assert!(sink.actions().is_empty());
    }

    #[test]
    fn missing_normal_key_release_does_not_block_hotkey() {
        let mut handler = HotkeyHandler::new(Some(command_left()), HotkeyMode::Toggle);
        let mut sink = TestSink::new();
        let none = |_: i64| false;
        handler.handle(RawEventType::KeyDown, 0, 0, &mut sink, none);
        handler.handle(
            RawEventType::FlagsChanged,
            55,
            MASK_COMMAND | 0x08,
            &mut sink,
            |_| false,
        );
        handler.handle(RawEventType::FlagsChanged, 55, 0, &mut sink, none);
        assert_eq!(sink.actions(), &["toggle"]);
    }

    #[test]
    fn keycode_hotkey_starts_and_stops_in_hold_mode() {
        let spec = parse_hotkey("keycode:49;mods:control+option").unwrap();
        let mut handler = HotkeyHandler::new(Some(spec), HotkeyMode::Hold);
        let mut sink = TestSink::new();
        let none = |_: i64| false;
        let mask = MASK_CONTROL | MASK_OPTION;
        handler.handle(RawEventType::KeyDown, 49, mask, &mut sink, none);
        handler.handle(RawEventType::KeyUp, 49, mask, &mut sink, none);
        assert_eq!(sink.actions(), &["start", "stop"]);
    }

    #[test]
    fn keycode_hotkey_release_does_not_retoggle() {
        let spec = parse_hotkey("keycode:49;mods:control+option").unwrap();
        let mut handler = HotkeyHandler::new(Some(spec), HotkeyMode::Toggle);
        let mut sink = TestSink::new();
        let none = |_: i64| false;
        let mask = MASK_CONTROL | MASK_OPTION;
        handler.handle(RawEventType::KeyDown, 49, mask, &mut sink, none);
        handler.handle(RawEventType::KeyUp, 49, mask, &mut sink, none);
        assert_eq!(sink.actions(), &["toggle"]);
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;

    #[test]
    fn backend_selection_matches_python() {
        let modifier = parse_hotkey("right_option");
        assert_eq!(backend_for_hotkey(modifier.as_ref(), "passive"), "passive");
        assert_eq!(backend_for_hotkey(modifier.as_ref(), "off"), "off");
        let key = parse_hotkey("keycode:49;mods:control+option");
        assert_eq!(backend_for_hotkey(key.as_ref(), "passive"), "registered");
        assert_eq!(backend_for_hotkey(key.as_ref(), ""), "registered");
        assert_eq!(backend_for_hotkey(key.as_ref(), "off"), "off");
        assert_eq!(backend_for_hotkey(None, "passive"), "passive");
        assert_eq!(backend_for_hotkey(None, "off"), "off");
    }

    #[test]
    fn display_labels_prefer_the_configured_shortcut_name() {
        assert_eq!(display_label("fn", "ignored"), "Fn / Globe");
        assert_eq!(display_label("right_option", "ignored"), "右 Option");
        assert_eq!(display_label("left_control", ""), "左 Control");
        assert_eq!(
            display_label("keycode:49;mods:control", "⌃Space"),
            "⌃Space"
        );
        assert_eq!(display_label("keycode:49;mods:control", ""), "自定义快捷键");
    }

    #[test]
    fn suppression_covers_only_the_configured_chord() {
        let spec = parse_hotkey("keycode:49;mods:control+option").unwrap();
        let (keys, mask) = suppression(Some(&spec));
        assert_eq!(keys, HashSet::from([49]));
        assert_eq!(mask, MASK_CONTROL | MASK_OPTION);

        let modifier = parse_hotkey("right_option").unwrap();
        let (keys, mask) = suppression(Some(&modifier));
        assert_eq!(keys, HashSet::from([61]));
        assert_eq!(mask, MASK_OPTION);

        assert_eq!(suppression(None), (HashSet::new(), 0));
    }

    #[test]
    fn shortcut_capture_pid_is_strict() {
        assert_eq!(shortcut_capture_pid(" 1234\n"), Some(1234));
        assert_eq!(shortcut_capture_pid("-1"), Some(-1));
        assert_eq!(shortcut_capture_pid(""), None);
        assert_eq!(shortcut_capture_pid("12 34"), None);
        assert_eq!(shortcut_capture_pid("abc"), None);
        assert_eq!(ESCAPE_KEYCODE, 53);
        assert_eq!(VALID_REGISTERED_MODIFIERS.len(), 5);
    }
}

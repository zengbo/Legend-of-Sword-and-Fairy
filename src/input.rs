//! Keyboard input state (port of SDLPAL input.c essentials): PALKEY press
//! bitmask plus the key-order based walking direction tracking.
#![allow(dead_code)] // used incrementally as engine bring-up proceeds

use crate::keys::KeyCode;

// PALKEY bits (input.h).
pub const KEY_MENU: u32 = 1 << 0;
pub const KEY_SEARCH: u32 = 1 << 1;
pub const KEY_DOWN: u32 = 1 << 2;
pub const KEY_LEFT: u32 = 1 << 3;
pub const KEY_UP: u32 = 1 << 4;
pub const KEY_RIGHT: u32 = 1 << 5;
pub const KEY_PGUP: u32 = 1 << 6;
pub const KEY_PGDN: u32 = 1 << 7;
pub const KEY_REPEAT: u32 = 1 << 8;
pub const KEY_AUTO: u32 = 1 << 9;
pub const KEY_DEFEND: u32 = 1 << 10;
pub const KEY_USE_ITEM: u32 = 1 << 11;
pub const KEY_THROW_ITEM: u32 = 1 << 12;
pub const KEY_FLEE: u32 = 1 << 13;
pub const KEY_STATUS: u32 = 1 << 14;
pub const KEY_FORCE: u32 = 1 << 15;
pub const KEY_HOME: u32 = 1 << 16;
pub const KEY_END: u32 = 1 << 17;

/// Directions (kDirSouth..kDirUnknown).
pub const DIR_SOUTH: usize = 0;
pub const DIR_WEST: usize = 1;
pub const DIR_NORTH: usize = 2;
pub const DIR_EAST: usize = 3;
pub const DIR_UNKNOWN: usize = 4;

/// The keyboard map (g_KeyMap): physical key -> PALKEY.
const KEY_MAP: &[(KeyCode, u32)] = &[
    (KeyCode::ArrowUp, KEY_UP),
    (KeyCode::Numpad8, KEY_UP),
    (KeyCode::ArrowDown, KEY_DOWN),
    (KeyCode::Numpad2, KEY_DOWN),
    (KeyCode::ArrowLeft, KEY_LEFT),
    (KeyCode::Numpad4, KEY_LEFT),
    (KeyCode::ArrowRight, KEY_RIGHT),
    (KeyCode::Numpad6, KEY_RIGHT),
    (KeyCode::Escape, KEY_MENU),
    (KeyCode::Insert, KEY_MENU),
    (KeyCode::AltLeft, KEY_MENU),
    (KeyCode::AltRight, KEY_MENU),
    (KeyCode::Numpad0, KEY_MENU),
    (KeyCode::Enter, KEY_SEARCH),
    (KeyCode::Space, KEY_SEARCH),
    (KeyCode::NumpadEnter, KEY_SEARCH),
    (KeyCode::ControlLeft, KEY_SEARCH),
    (KeyCode::PageUp, KEY_PGUP),
    (KeyCode::Numpad9, KEY_PGUP),
    (KeyCode::PageDown, KEY_PGDN),
    (KeyCode::Numpad3, KEY_PGDN),
    (KeyCode::Home, KEY_HOME),
    (KeyCode::Numpad7, KEY_HOME),
    (KeyCode::End, KEY_END),
    (KeyCode::Numpad1, KEY_END),
    (KeyCode::KeyR, KEY_REPEAT),
    (KeyCode::KeyA, KEY_AUTO),
    (KeyCode::KeyD, KEY_DEFEND),
    (KeyCode::KeyE, KEY_USE_ITEM),
    (KeyCode::KeyW, KEY_THROW_ITEM),
    (KeyCode::KeyQ, KEY_FLEE),
    (KeyCode::KeyF, KEY_FORCE),
    (KeyCode::KeyS, KEY_STATUS),
];

const KEY_MAP_LEN: usize = 33;

/// PAL_INPUT_STATE, tracking pressed keys the way PAL_UpdateKeyboardState
/// does (poll pressed state, key-repeat timing, direction ordering).
pub struct InputState {
    pub dir: usize,
    pub prev_dir: usize,
    pub key_press: u32,

    /// Most recent direction press that has not yet been consumed by a game
    /// frame. This preserves a quick press+release that occurs entirely
    /// between two 100 ms movement frames.
    pending_dir: usize,

    /// Which KEY_MAP entries are physically held down.
    down: [bool; KEY_MAP_LEN],
    /// Per-entry next-repeat deadline in ms (0 = not pressed yet).
    last_time: [u64; KEY_MAP_LEN],
    key_order: [u32; 4],
    key_max_count: u32,
    /// Enable held-key repeat like gConfig.fEnableKeyRepeat.
    pub enable_key_repeat: bool,
}

impl Default for InputState {
    fn default() -> InputState {
        InputState {
            dir: DIR_UNKNOWN,
            prev_dir: DIR_UNKNOWN,
            key_press: 0,
            pending_dir: DIR_UNKNOWN,
            down: [false; KEY_MAP_LEN],
            last_time: [0; KEY_MAP_LEN],
            key_order: [0; 4],
            key_max_count: 0,
            enable_key_repeat: true,
        }
    }
}

impl InputState {
    pub fn new() -> InputState {
        InputState::default()
    }

    /// Record a physical key state change from the window system.
    pub fn handle_key_event(&mut self, code: KeyCode, pressed: bool) {
        for (i, (key, mapped)) in KEY_MAP.iter().enumerate() {
            if *key == code {
                if pressed && !self.down[i] {
                    let dir = Self::dir_of_key(*mapped);
                    if dir != DIR_UNKNOWN {
                        self.pending_dir = dir;
                    }
                }
                self.down[i] = pressed;
            }
        }
    }

    /// Record a discrete key tap from a source that cannot report key-up.
    ///
    /// Direction taps are kept for exactly one movement/input cycle. Multiple
    /// terminal repeat sequences received before that cycle are coalesced,
    /// preventing queued movement after the user has released the key.
    pub fn handle_key_tap(&mut self, code: KeyCode) {
        for &(key, mapped) in KEY_MAP {
            if key == code {
                self.key_press |= mapped;
                let dir = Self::dir_of_key(mapped);
                if dir != DIR_UNKNOWN {
                    self.pending_dir = dir;
                }
            }
        }
    }

    /// Current direction for non-movement consumers such as the battle UI.
    pub fn direction(&self) -> usize {
        if self.pending_dir != DIR_UNKNOWN {
            self.pending_dir
        } else {
            self.dir
        }
    }

    /// Direction for one party movement frame. A pending tap is consumed;
    /// a physically held key remains active on subsequent frames.
    pub fn take_direction(&mut self) -> usize {
        let pending = std::mem::replace(&mut self.pending_dir, DIR_UNKNOWN);
        if pending != DIR_UNKNOWN {
            pending
        } else {
            self.dir
        }
    }

    /// PAL_GetCurrDirection.
    fn current_direction(&self) -> usize {
        let mut cur = DIR_SOUTH;
        for i in 1..4 {
            if self.key_order[cur] < self.key_order[i] {
                cur = i;
            }
        }
        if self.key_order[cur] == 0 {
            DIR_UNKNOWN
        } else {
            cur
        }
    }

    fn dir_of_key(key: u32) -> usize {
        if key & KEY_DOWN != 0 {
            DIR_SOUTH
        } else if key & KEY_LEFT != 0 {
            DIR_WEST
        } else if key & KEY_UP != 0 {
            DIR_NORTH
        } else if key & KEY_RIGHT != 0 {
            DIR_EAST
        } else {
            DIR_UNKNOWN
        }
    }

    /// PAL_KeyDown.
    fn key_down(&mut self, key: u32, repeat: bool) {
        if !repeat {
            let cur = Self::dir_of_key(key);
            if cur != DIR_UNKNOWN {
                self.key_max_count += 1;
                self.key_order[cur] = self.key_max_count;
                self.dir = self.current_direction();
            }
        }
        self.key_press |= key;
    }

    /// PAL_KeyUp.
    fn key_up(&mut self, key: u32) {
        let cur = Self::dir_of_key(key);
        if cur != DIR_UNKNOWN {
            self.key_order[cur] = 0;
            let new_dir = self.current_direction();
            self.key_max_count = if new_dir == DIR_UNKNOWN {
                0
            } else {
                self.key_order[new_dir]
            };
            self.dir = new_dir;
        }
    }

    /// PAL_UpdateKeyboardState: poll the held-key table and generate
    /// key-down/up transitions with repeat timing. `now_ms` is a monotonic
    /// millisecond clock.
    pub fn update_keyboard_state(&mut self, now_ms: u64) {
        for (i, &(_, key)) in KEY_MAP.iter().enumerate() {
            if self.down[i] {
                if now_ms > self.last_time[i] {
                    let first = self.last_time[i] == 0;
                    self.key_down(key, !first);
                    self.last_time[i] = if self.enable_key_repeat {
                        now_ms + if first { 200 } else { 75 }
                    } else {
                        u64::MAX
                    };
                }
            } else if self.last_time[i] != 0 {
                self.key_up(key);
                self.last_time[i] = 0;
            }
        }
    }

    /// PAL_ClearKeyState.
    pub fn clear_key_state(&mut self) {
        self.key_press = 0;
        self.pending_dir = DIR_UNKNOWN;
    }

    /// Reset the walking direction (used by fades: dir = prevdir = unknown).
    pub fn reset_dir(&mut self) {
        self.dir = DIR_UNKNOWN;
        self.prev_dir = DIR_UNKNOWN;
        self.pending_dir = DIR_UNKNOWN;
    }

    /// Test whether any of `keys` was pressed since the last clear.
    pub fn pressed(&self, keys: u32) -> bool {
        self.key_press & keys != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_map_len_constant_matches() {
        assert_eq!(KEY_MAP.len(), KEY_MAP_LEN);
    }

    #[test]
    fn direction_ordering_follows_most_recent_key() {
        let mut s = InputState::new();
        s.handle_key_event(KeyCode::ArrowUp, true);
        s.update_keyboard_state(10);
        assert_eq!(s.dir, DIR_NORTH);
        assert!(s.pressed(KEY_UP));

        // Press right while up held: direction switches to east.
        s.handle_key_event(KeyCode::ArrowRight, true);
        s.update_keyboard_state(20);
        assert_eq!(s.dir, DIR_EAST);

        // Release right: direction falls back to north.
        s.handle_key_event(KeyCode::ArrowRight, false);
        s.update_keyboard_state(30);
        assert_eq!(s.dir, DIR_NORTH);

        // Release up: no direction.
        s.handle_key_event(KeyCode::ArrowUp, false);
        s.update_keyboard_state(40);
        assert_eq!(s.dir, DIR_UNKNOWN);
    }

    #[test]
    fn key_press_is_edge_triggered_with_repeat() {
        let mut s = InputState::new();
        s.handle_key_event(KeyCode::Enter, true);
        s.update_keyboard_state(10);
        assert!(s.pressed(KEY_SEARCH));
        s.clear_key_state();
        // Held but within repeat delay: no new press.
        s.update_keyboard_state(100);
        assert!(!s.pressed(KEY_SEARCH));
        // After the 200ms initial repeat delay: repeats.
        s.update_keyboard_state(300);
        assert!(s.pressed(KEY_SEARCH));
    }

    #[test]
    fn terminal_direction_tap_is_consumed_exactly_once() {
        let mut s = InputState::new();
        s.handle_key_tap(KeyCode::ArrowRight);
        assert!(s.pressed(KEY_RIGHT));
        assert_eq!(s.direction(), DIR_EAST);
        assert_eq!(s.take_direction(), DIR_EAST);
        assert_eq!(s.take_direction(), DIR_UNKNOWN);
    }

    #[test]
    fn repeated_terminal_taps_coalesce_before_a_frame() {
        let mut s = InputState::new();
        s.handle_key_tap(KeyCode::ArrowDown);
        s.handle_key_tap(KeyCode::ArrowDown);
        s.handle_key_tap(KeyCode::ArrowDown);
        assert_eq!(s.take_direction(), DIR_SOUTH);
        assert_eq!(s.take_direction(), DIR_UNKNOWN);
    }

    #[test]
    fn quick_physical_press_and_release_still_moves_once() {
        let mut s = InputState::new();
        s.handle_key_event(KeyCode::ArrowLeft, true);
        s.handle_key_event(KeyCode::ArrowLeft, false);
        s.update_keyboard_state(10);
        assert_eq!(s.dir, DIR_UNKNOWN);
        assert_eq!(s.take_direction(), DIR_WEST);
        assert_eq!(s.take_direction(), DIR_UNKNOWN);
    }

    #[test]
    fn held_direction_continues_after_initial_latch() {
        let mut s = InputState::new();
        s.handle_key_event(KeyCode::ArrowUp, true);
        s.update_keyboard_state(10);
        assert_eq!(s.take_direction(), DIR_NORTH);
        assert_eq!(s.take_direction(), DIR_NORTH);
        s.handle_key_event(KeyCode::ArrowUp, false);
        s.update_keyboard_state(20);
        assert_eq!(s.take_direction(), DIR_UNKNOWN);
    }
}

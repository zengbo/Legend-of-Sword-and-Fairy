//! Engine key codes, independent of winit.
//!
//! The GUI backend maps winit `KeyCode`s into these; the console backend maps
//! terminal escape sequences; the web backend uses the same indices as
//! `WEB_KEYS` in `web.rs` / `KEY_CODES` in `web/main.js`.

/// Physical keys the engine cares about (a subset of winit's KeyCode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Escape,
    Insert,
    Enter,
    Space,
    ControlLeft,
    PageUp,
    PageDown,
    Home,
    End,
    KeyR,
    KeyA,
    KeyD,
    KeyE,
    KeyW,
    KeyQ,
    KeyF,
    KeyS,
    AltLeft,
    AltRight,
    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,
    NumpadEnter,
}

/// Map a winit key into the engine set. Returns `None` for keys we ignore.
#[cfg(all(not(target_arch = "wasm32"), feature = "gui"))]
pub fn from_winit(code: winit::keyboard::KeyCode) -> Option<KeyCode> {
    use winit::keyboard::KeyCode as W;
    Some(match code {
        W::ArrowUp => KeyCode::ArrowUp,
        W::ArrowDown => KeyCode::ArrowDown,
        W::ArrowLeft => KeyCode::ArrowLeft,
        W::ArrowRight => KeyCode::ArrowRight,
        W::Escape => KeyCode::Escape,
        W::Insert => KeyCode::Insert,
        W::Enter => KeyCode::Enter,
        W::Space => KeyCode::Space,
        W::ControlLeft => KeyCode::ControlLeft,
        W::PageUp => KeyCode::PageUp,
        W::PageDown => KeyCode::PageDown,
        W::Home => KeyCode::Home,
        W::End => KeyCode::End,
        W::KeyR => KeyCode::KeyR,
        W::KeyA => KeyCode::KeyA,
        W::KeyD => KeyCode::KeyD,
        W::KeyE => KeyCode::KeyE,
        W::KeyW => KeyCode::KeyW,
        W::KeyQ => KeyCode::KeyQ,
        W::KeyF => KeyCode::KeyF,
        W::KeyS => KeyCode::KeyS,
        W::AltLeft => KeyCode::AltLeft,
        W::AltRight => KeyCode::AltRight,
        W::Numpad0 => KeyCode::Numpad0,
        W::Numpad1 => KeyCode::Numpad1,
        W::Numpad2 => KeyCode::Numpad2,
        W::Numpad3 => KeyCode::Numpad3,
        W::Numpad4 => KeyCode::Numpad4,
        W::Numpad6 => KeyCode::Numpad6,
        W::Numpad7 => KeyCode::Numpad7,
        W::Numpad8 => KeyCode::Numpad8,
        W::Numpad9 => KeyCode::Numpad9,
        W::NumpadEnter => KeyCode::NumpadEnter,
        _ => return None,
    })
}

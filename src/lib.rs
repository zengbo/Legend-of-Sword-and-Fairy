//! rustpal — a Rust reimplementation of the PAL (仙剑奇侠传 / Legend of
//! Sword and Fairy) DOS engine, ported from SDLPAL.

pub mod audio;
pub mod battle;
pub mod data;
pub mod ending;
pub mod fight;
pub mod font;
pub mod game_loop;
pub mod global;
pub mod input;
pub mod itemmenu;
pub mod keys;
pub mod magicmenu;
pub mod map;
pub mod mkf;
#[cfg(all(not(target_arch = "wasm32"), feature = "gui"))]
pub mod native_upscale;
pub mod opl;
pub mod palette;
pub mod play;
pub mod res;
pub mod rix;
pub mod rngplay;
pub mod scene;
pub mod script;
pub mod surface;
pub mod text;
pub mod ui;
// HTTP control API for GUI and/or console (needs `png` via either feature).
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "gui", feature = "console")
))]
pub mod ui_driver;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "gui", feature = "console")
))]
pub(crate) mod agent_state;
pub mod uibattle;
pub mod uigame;
#[cfg(all(not(target_arch = "wasm32"), feature = "console"))]
pub(crate) mod hqx;
#[cfg(all(not(target_arch = "wasm32"), feature = "console"))]
pub mod video_console;
pub mod voc;
#[cfg(target_arch = "wasm32")]
pub mod web;
pub mod yj;

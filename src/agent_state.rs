//! Observation JSON for AI / HTTP driver.
//!
//! - `GET /v1/state` — lightweight: position, walk, map (dirs/exits/obstacles/
//!   mechanisms), nearby events, dialog/menu/battle.
//! - `GET /v1/party` / `GET /v1/inventory` — on demand (not every poll).
//!
//! **Facts only** — no recommended path or "press this" strategy.
//! Strategy is the AI client's job.

use std::sync::Mutex;

use crate::battle::{BattleMenuState, BattlePhase, BattleUiState, FighterState};
use crate::game_loop::Engine;
use crate::global::{
    ITEMFLAG_APPLY_TO_ALL, ITEMFLAG_CONSUMING, ITEMFLAG_EQUIPABLE, ITEMFLAG_SELLABLE,
    ITEMFLAG_THROWABLE, ITEMFLAG_USABLE, MAGICFLAG_APPLY_TO_ALL, MAGICFLAG_USABLE_IN_BATTLE,
    MAGICFLAG_USABLE_OUTSIDE_BATTLE, MAGICFLAG_USABLE_TO_ENEMY, MAX_ENEMIES_IN_TEAM,
    MAX_INVENTORY, MAX_LEVELS, MAX_PLAYABLE_PLAYER_ROLES, MAX_PLAYER_EQUIPMENTS, MAX_PLAYER_MAGICS,
    MAX_PLAYER_ROLES, MAX_PLAYERS_IN_PARTY, OBJSTATE_BLOCKER, STATUS_ALL,
};
use crate::map::{MAP_HEIGHT, MAP_WIDTH};
use crate::ui::{agent_text_from_bytes, AgentMenuItem};
use crate::ui_driver;

/// Cached blocked-tile list for the current map_num (rebuilt on map change).
static OBSTACLE_TILE_CACHE: Mutex<Option<(usize, String)>> = Mutex::new(None);

/// Overworld step deltas matching `play` / key mapping:
/// key order is NOT party_direction order — see `DIR_KEYS`.
const WALK_DELTA: [((i32, i32), &str); 4] = [
    ((16, -8), "up"),    // DIR_NORTH = 2
    ((16, 8), "right"),  // DIR_EAST  = 3
    ((-16, 8), "down"),  // DIR_SOUTH = 0
    ((-16, -8), "left"), // DIR_WEST  = 1
];

/// party_direction → key name (DIR_SOUTH/WEST/NORTH/EAST = 0..3).
const DIR_KEYS: [&str; 4] = ["down", "left", "up", "right"];

/// Max nearby event objects listed (sorted by distance).
const MAX_EVENTS: usize = 48;
/// Max inventory rows listed.
const MAX_INV_LIST: usize = 64;
/// Status short names (STATUS_* index).
const STATUS_NAMES: [&str; STATUS_ALL] = [
    "conf", "para", "sleep", "silence", "puppet", "brave", "prot", "haste", "dual",
];

pub(crate) fn build_state_json(engine: &Engine) -> String {
    let frame_id = ui_driver::latest_frame_id();
    let step_mode = ui_driver::step_mode_enabled();
    let step_configured = ui_driver::step_mode_configured();
    let g = &engine.globals;
    let player = (
        g.viewport.0 + g.partyoffset.0,
        g.viewport.1 + g.partyoffset.1,
    );
    let in_dialog = engine.ui.in_dialog
        || engine.ui.current_dialog_line > 0
        || !engine.ui.agent_dialog_lines.is_empty()
        || !engine.ui.agent_dialog_speaker.is_empty();
    let in_battle = g.in_battle || engine.battle.is_some();
    let in_menu = engine.ui.agent_menu.is_some()
        || engine
            .battle
            .as_ref()
            .is_some_and(|b| b.ui.state == BattleUiState::SelectMove);

    let phase = if !g.in_main_game && !in_menu {
        "boot"
    } else if in_battle {
        "battle"
    } else if in_dialog {
        "dialog"
    } else if in_menu {
        "menu"
    } else if g.entering_scene {
        "scene_transition"
    } else {
        "overworld"
    };

    let walk = compute_walk(engine, player);

    let mut out = String::with_capacity(12 << 10);
    out.push('{');
    push_str(&mut out, "status", "ok");
    out.push(',');
    push_u64(&mut out, "frame_id", frame_id);
    out.push(',');
    push_bool(&mut out, "step_mode", step_mode);
    out.push(',');
    push_bool(&mut out, "step_configured", step_configured);
    out.push(',');
    push_u64(&mut out, "ticks", engine.ticks());
    out.push(',');
    push_u64(&mut out, "frame_num", g.frame_num as u64);
    out.push(',');
    push_str(&mut out, "phase", phase);
    out.push(',');
    push_u64(&mut out, "scene", g.num_scene as u64);
    out.push(',');
    push_pair(&mut out, "viewport", g.viewport.0, g.viewport.1);
    out.push(',');
    push_pair(&mut out, "player", player.0, player.1);
    out.push(',');
    push_u64(&mut out, "party_direction", g.party_direction as u64);
    out.push(',');
    // Key name for current facing (down/left/up/right).
    push_str(&mut out, "facing", dir_to_key(g.party_direction));
    out.push(',');
    push_bool(&mut out, "in_main_game", g.in_main_game);
    out.push(',');
    push_bool(&mut out, "entering_scene", g.entering_scene);
    out.push(',');
    push_bool(&mut out, "need_to_fade_in", g.need_to_fade_in);
    out.push(',');
    push_bool(&mut out, "in_battle", in_battle);
    out.push(',');
    push_bool(&mut out, "auto_battle", g.auto_battle);
    out.push(',');
    // Live dialog: single UTF-8 string (or null). Prefer this over in_dialog.
    out.push_str("\"dialog\":");
    append_dialog(&mut out, engine);
    out.push(',');
    // Live menu (null when none). Prefer this over in_menu.
    out.push_str("\"menu\":");
    append_menu(&mut out, engine);
    out.push(',');
    push_bool(&mut out, "quit_requested", engine.quit_requested);
    out.push(',');
    push_u64(&mut out, "cash", g.cash as u64);
    out.push(',');
    push_u64(&mut out, "collect_value", g.collect_value as u64);
    out.push(',');
    push_u64(&mut out, "current_save_slot", g.current_save_slot as u64);
    out.push(',');
    push_u64(&mut out, "playtime_secs", engine.playtime_secs());
    out.push(',');
    push_u64(&mut out, "palette", g.num_palette as u64);
    out.push(',');
    push_bool(&mut out, "night_palette", g.night_palette);
    out.push(',');
    push_u64(&mut out, "music", g.num_music as u64);
    out.push(',');
    push_u64(&mut out, "battle_music", g.num_battle_music as u64);
    out.push(',');
    push_u64(&mut out, "battle_field", g.num_battle_field as u64);
    out.push(',');
    push_i64(&mut out, "cur_main_menu_item", g.cur_main_menu_item as i64);
    out.push(',');
    push_i64(&mut out, "cur_system_menu_item", g.cur_system_menu_item as i64);
    out.push(',');
    push_i64(&mut out, "cur_inv_menu_item", g.cur_inv_menu_item as i64);
    out.push(',');
    push_u64(
        &mut out,
        "max_party_member_index",
        g.max_party_member_index as u64,
    );

    // Scene metadata.
    out.push(',');
    out.push_str("\"scene_info\":");
    append_scene_info(&mut out, engine);

    // Four-way walkability from current tile.
    out.push(',');
    out.push_str("\"walk\":{");
    for (i, &((_, _), name)) in WALK_DELTA.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_bool(&mut out, name, walk[i]);
    }
    out.push('}');

    // Scene geometry: key→world steps, exits, mechanisms, obstacles.
    out.push(',');
    out.push_str("\"map\":");
    append_map(&mut out, engine, player);

    // Nearby event objects — facts only.
    out.push(',');
    out.push_str("\"events\":");
    out.push_str(&build_events_json(engine, player));

    // Battle block (null when not in battle).
    out.push(',');
    out.push_str("\"battle\":");
    if let Some(battle) = engine.battle.as_ref() {
        append_battle(&mut out, engine, battle);
    } else {
        out.push_str("null");
    }

    // Pointers to heavy on-demand resources.
    out.push(',');
    out.push_str("\"resources\":{\"party\":\"/v1/party\",\"inventory\":\"/v1/inventory\"}");

    // Legal input vocabulary (not a suggestion of what to press).
    out.push(',');
    out.push_str(
        "\"actions\":[\"up\",\"down\",\"left\",\"right\",\"confirm\",\"space\",\"menu\",\
         \"force\",\"auto\",\"defend\",\"use_item\",\"throw_item\",\"flee\",\"status\",\
         \"repeat\",\"page_up\",\"page_down\",\"home\",\"end\"]",
    );

    out.push('}');
    out.push('\n');
    out
}

/// `GET /v1/party` body.
pub(crate) fn build_party_json(engine: &Engine) -> String {
    let mut out = String::with_capacity(4 << 10);
    out.push('{');
    push_str(&mut out, "status", "ok");
    out.push(',');
    push_u64(&mut out, "frame_id", ui_driver::latest_frame_id());
    out.push(',');
    out.push_str("\"party\":");
    append_party(&mut out, engine);
    out.push('}');
    out.push('\n');
    out
}

/// `GET /v1/inventory` body.
pub(crate) fn build_inventory_json(engine: &Engine) -> String {
    let mut out = String::with_capacity(2 << 10);
    out.push('{');
    push_str(&mut out, "status", "ok");
    out.push(',');
    push_u64(&mut out, "frame_id", ui_driver::latest_frame_id());
    out.push(',');
    out.push_str("\"inventory\":");
    append_inventory(&mut out, engine);
    out.push('}');
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Dialog / menu
// ---------------------------------------------------------------------------

/// One UTF-8 string for the current dialog page, or `null`.
fn append_dialog(out: &mut String, engine: &Engine) {
    let speaker = &engine.ui.agent_dialog_speaker;
    let lines = &engine.ui.agent_dialog_lines;
    if speaker.is_empty() && lines.is_empty() {
        out.push_str("null");
        return;
    }
    let mut full = String::new();
    if !speaker.is_empty() {
        full.push_str(speaker);
        full.push_str("：");
    }
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            full.push('\n');
        }
        full.push_str(line);
    }
    push_json_string(out, &full);
}

fn append_menu(out: &mut String, engine: &Engine) {
    if let Some(menu) = engine.ui.agent_menu.as_ref() {
        write_menu_obj(out, &menu.kind, menu.index, &menu.items);
        return;
    }
    if let Some(battle) = engine.battle.as_ref() {
        if battle.ui.state == BattleUiState::SelectMove {
            let (kind, index, items) = battle_menu_snapshot(engine, battle);
            write_menu_obj(out, kind, index, &items);
            return;
        }
    }
    out.push_str("null");
}

fn write_menu_obj(out: &mut String, kind: &str, index: usize, items: &[AgentMenuItem]) {
    out.push('{');
    push_str(out, "kind", kind);
    out.push(',');
    push_u64(out, "index", index as u64);
    out.push(',');
    out.push_str("\"items\":[");
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('{');
        push_u64(out, "value", it.value as u64);
        out.push(',');
        push_str(out, "label", &it.label);
        if !it.enabled {
            out.push(',');
            push_bool(out, "enabled", false);
        }
        out.push('}');
    }
    out.push(']');
    out.push('}');
}

/// Classic battle command menu (synthesized; live item/magic lists use agent_menu).
fn battle_menu_snapshot(
    engine: &Engine,
    battle: &crate::battle::Battle,
) -> (&'static str, usize, Vec<AgentMenuItem>) {
    const LABEL_USEITEM: u16 = 23;
    const LABEL_THROWITEM: u16 = 24;
    const LABEL_AUTO: u16 = 56;
    const LABEL_INVENTORY: u16 = 57;
    const LABEL_DEFEND: u16 = 58;
    const LABEL_FLEE: u16 = 59;
    const LABEL_STATUS: u16 = 60;
    const LABEL_MAGIC: u16 = 14;

    let word = |id: u16| agent_text_from_bytes(&engine.texts.word(id as usize));
    let lab = |id: u16, fb: &str| {
        let s = word(id);
        if s.is_empty() {
            fb.to_string()
        } else {
            s
        }
    };

    match battle.ui.menu_state {
        BattleMenuState::Main => {
            let items = vec![
                AgentMenuItem {
                    value: 0,
                    label: "攻擊".into(),
                    enabled: true,
                },
                AgentMenuItem {
                    value: 1,
                    label: lab(LABEL_MAGIC, "法術"),
                    enabled: true,
                },
                AgentMenuItem {
                    value: 2,
                    label: "協力".into(),
                    enabled: true,
                },
                AgentMenuItem {
                    value: 3,
                    label: "其它".into(),
                    enabled: true,
                },
            ];
            (
                "battle_main",
                (battle.ui.selected_action as usize) % 4,
                items,
            )
        }
        BattleMenuState::Misc => {
            let items = vec![
                AgentMenuItem {
                    value: 0,
                    label: lab(LABEL_AUTO, "自動"),
                    enabled: true,
                },
                AgentMenuItem {
                    value: 1,
                    label: lab(LABEL_INVENTORY, "道具"),
                    enabled: true,
                },
                AgentMenuItem {
                    value: 2,
                    label: lab(LABEL_DEFEND, "防禦"),
                    enabled: true,
                },
                AgentMenuItem {
                    value: 3,
                    label: lab(LABEL_FLEE, "逃跑"),
                    enabled: true,
                },
                AgentMenuItem {
                    value: 4,
                    label: lab(LABEL_STATUS, "狀態"),
                    enabled: true,
                },
            ];
            (
                "battle_misc",
                battle.ui.selected_index.clamp(0, 4) as usize,
                items,
            )
        }
        BattleMenuState::MiscItemSubMenu => {
            let items = vec![
                AgentMenuItem {
                    value: 0,
                    label: lab(LABEL_USEITEM, "使用"),
                    enabled: true,
                },
                AgentMenuItem {
                    value: 1,
                    label: lab(LABEL_THROWITEM, "投擲"),
                    enabled: true,
                },
            ];
            (
                "battle_item_sub",
                battle.ui.selected_index.clamp(0, 1) as usize,
                items,
            )
        }
        BattleMenuState::MagicSelect
        | BattleMenuState::UseItemSelect
        | BattleMenuState::ThrowItemSelect => ("battle", 0, Vec::new()),
    }
}

// ---------------------------------------------------------------------------
// Scene / party / inventory
// ---------------------------------------------------------------------------

fn append_scene_info(out: &mut String, engine: &Engine) {
    let g = &engine.globals;
    let scene_i = g.num_scene as usize;
    if scene_i == 0 || scene_i > g.game.scenes.len() {
        out.push_str("null");
        return;
    }
    let sc = g.game.scenes[scene_i - 1];
    let end = g
        .game
        .scenes
        .get(scene_i)
        .map(|s| s.event_object_index as usize)
        .unwrap_or(g.game.event_objects.len());
    let start = sc.event_object_index as usize;
    let event_count = end.saturating_sub(start);
    out.push('{');
    push_u64(out, "map_num", sc.map_num as u64);
    out.push(',');
    push_u64(out, "script_on_enter", sc.script_on_enter as u64);
    out.push(',');
    push_u64(out, "script_on_teleport", sc.script_on_teleport as u64);
    out.push(',');
    push_u64(out, "event_object_index", sc.event_object_index as u64);
    out.push(',');
    push_u64(out, "event_count", event_count as u64);
    out.push('}');
}

/// Current-scene geometry for pathfinding (facts only).
///
/// - `dirs`: what each move key does in world coords (isometric).
/// - `exits`: doors/teleports **of this scene only** (not other rooms of a maze).
/// - `mechanisms`: switches / inspectables that need walk-to + confirm (not pure decor).
/// - `obstacles`: blocked map tiles + event objects that block walking.
fn append_map(out: &mut String, engine: &Engine, player: (i32, i32)) {
    let g = &engine.globals;
    out.push('{');

    // --- dirs: key → world step (same as walk / play) ---
    out.push_str(
        "\"dirs\":{\
         \"up\":{\"dx\":16,\"dy\":-8,\"world\":\"(+16,-8)\"},\
         \"right\":{\"dx\":16,\"dy\":8,\"world\":\"(+16,+8)\"},\
         \"down\":{\"dx\":-16,\"dy\":8,\"world\":\"(-16,+8)\"},\
         \"left\":{\"dx\":-16,\"dy\":-8,\"world\":\"(-16,-8)\"}\
         }",
    );
    out.push(',');
    push_str(
        out,
        "coord_note",
        "player/events/exits use world [x,y]. One walk key moves by dirs[key]. \
         Not screen pixels. dist metric = |dx|+2*|dy|.",
    );

    // --- current scene event range ---
    let scene_i = g.num_scene as usize;
    let (start, end) = if scene_i >= 1 && scene_i <= g.game.scenes.len() {
        let s = g.game.scenes[scene_i - 1].event_object_index as usize;
        let e = g
            .game
            .scenes
            .get(scene_i)
            .map(|sc| sc.event_object_index as usize)
            .unwrap_or(g.game.event_objects.len());
        (s, e.min(g.game.event_objects.len()))
    } else {
        (0, 0)
    };
    let party_dir = g.party_direction;

    // --- exits (this scene only) ---
    out.push(',');
    out.push_str("\"exits\":[");
    let mut first_exit = true;
    for index in start..end {
        let ev = g.game.event_objects[index];
        if ev.state <= 0 || ev.vanish_time != 0 || ev.trigger_script == 0 {
            continue;
        }
        let Some(dest) = script_destination_scene(engine, ev.trigger_script) else {
            continue;
        };
        if !first_exit {
            out.push(',');
        }
        first_exit = false;
        let event_id = (index + 1) as u16;
        let pos = (ev.x as i32, ev.y as i32);
        let kind = if ev.trigger_mode >= 4 {
            "touch"
        } else if ev.trigger_mode == 0 {
            "scenery"
        } else {
            "search"
        };
        out.push('{');
        push_u64(out, "id", event_id as u64);
        out.push(',');
        push_str(out, "kind", kind);
        out.push(',');
        push_pair(out, "pos", pos.0, pos.1);
        out.push(',');
        push_pair(out, "delta", pos.0 - player.0, pos.1 - player.1);
        out.push(',');
        push_i64(out, "dist", metric(player, pos) as i64);
        out.push(',');
        push_u64(out, "dest_scene", dest as u64);
        out.push(',');
        push_u64(out, "trigger_mode", ev.trigger_mode as u64);
        out.push(',');
        push_str(out, "how", if kind == "touch" { "walk_into" } else { "face_and_confirm" });
        out.push('}');
    }
    out.push(']');

    // --- mechanisms: interactable non-exit (switches, chests, levers, NPCs to talk) ---
    out.push(',');
    out.push_str("\"mechanisms\":[");
    let mut first_m = true;
    for index in start..end {
        let ev = g.game.event_objects[index];
        if ev.state <= 0 || ev.vanish_time != 0 || ev.trigger_script == 0 {
            continue;
        }
        if script_destination_scene(engine, ev.trigger_script).is_some() {
            continue; // exits listed separately
        }
        // Skip pure scenery with no useful trigger mode.
        if ev.trigger_mode == 0 {
            continue;
        }
        let rank = analyze_script_progress(engine, ev.trigger_script);
        let event_id = (index + 1) as u16;
        let item_use = story_item_for_event(engine, event_id);
        // Keep dialog NPCs as mechanisms too (talk = confirm); tag progress.
        let pos = (ev.x as i32, ev.y as i32);
        let kind = if ev.trigger_mode >= 4 {
            "touch"
        } else {
            "search"
        };
        let (search_ok, face) = if ev.trigger_mode > 0 && ev.trigger_mode < 4 {
            search_status(player, pos, ev.trigger_mode, party_dir)
        } else {
            (false, None)
        };
        let touch_radius = if ev.trigger_mode >= 4 {
            ((ev.trigger_mode - 4) as i32 * 32 + 16).max(16)
        } else {
            0
        };
        let in_touch = touch_radius > 0 && metric(player, pos) < touch_radius;
        if !first_m {
            out.push(',');
        }
        first_m = false;
        out.push('{');
        push_u64(out, "id", event_id as u64);
        out.push(',');
        push_str(out, "kind", kind);
        out.push(',');
        let role = classify_event_role(ev.trigger_mode, ev.sprite_num, ev.trigger_script, None);
        push_str(out, "role", role);
        out.push(',');
        push_pair(out, "pos", pos.0, pos.1);
        out.push(',');
        push_pair(out, "delta", pos.0 - player.0, pos.1 - player.1);
        out.push(',');
        push_i64(out, "dist", metric(player, pos) as i64);
        out.push(',');
        push_str(out, "progress", if item_use.is_some() { "item" } else { rank.label });
        out.push(',');
        push_str(
            out,
            "how",
            if kind == "touch" {
                "walk_into"
            } else {
                "face_and_confirm"
            },
        );
        out.push(',');
        push_bool(out, "can_search_now", search_ok);
        out.push(',');
        push_bool(out, "in_touch_range", in_touch);
        if let Some(f) = face {
            out.push(',');
            push_str(out, "face", f);
            out.push(',');
            push_bool(out, "in_search_range", true);
        }
        if let Some(item) = item_use {
            out.push(',');
            push_u64(out, "item_use", item as u64);
        }
        if rank.rank >= 5 {
            out.push(',');
            push_bool(out, "loop", true);
        }
        out.push('}');
    }
    out.push(']');

    // --- obstacles: map tiles + event blockers ---
    out.push(',');
    out.push_str("\"obstacles\":{");
    // Tile blocks (cached per map_num).
    out.push_str("\"tiles\":");
    append_blocked_tiles(out, engine);
    out.push(',');
    // Event objects that act as solid blockers (NPCs standing in the way, etc.).
    out.push_str("\"event_blockers\":[");
    let mut first_b = true;
    for index in start..end {
        let ev = g.game.event_objects[index];
        if ev.state < OBJSTATE_BLOCKER || ev.vanish_time != 0 {
            continue;
        }
        if !first_b {
            out.push(',');
        }
        first_b = false;
        out.push('{');
        push_u64(out, "id", (index + 1) as u64);
        out.push(',');
        push_pair(out, "pos", ev.x as i32, ev.y as i32);
        out.push(',');
        push_i64(out, "state", ev.state as i64);
        out.push('}');
    }
    out.push(']');
    out.push(',');
    push_str(
        out,
        "tile_note",
        "tiles are map [x,y,h] (x:0..63 y:0..127 h:0|1). \
         world ≈ (x*32+h*16, y*16+h*8). Event blockers solid when state>=2.",
    );
    out.push('}');

    out.push('}');
}

fn append_blocked_tiles(out: &mut String, engine: &Engine) {
    let Some(map) = engine.res.map.as_ref() else {
        out.push_str("[]");
        return;
    };
    let map_num = map.num;
    // Reuse cache when map unchanged.
    if let Ok(guard) = OBSTACLE_TILE_CACHE.lock() {
        if let Some((n, ref s)) = *guard {
            if n == map_num {
                out.push_str(s);
                return;
            }
        }
    }
    let mut tiles = String::from("[");
    let mut first = true;
    let mut count = 0usize;
    // Cap to keep JSON reasonable; full maps rarely need every blocked cell
    // beyond ~8k. Prefer dense blocked listing over open cells.
    const MAX_BLOCKED: usize = 12_000;
    for y in 0..MAP_HEIGHT {
        for x in 0..MAP_WIDTH {
            for h in 0u8..2 {
                if map.tile_is_blocked(x as u8, y as u8, h) {
                    if count >= MAX_BLOCKED {
                        break;
                    }
                    if !first {
                        tiles.push(',');
                    }
                    first = false;
                    tiles.push('[');
                    tiles.push_str(&x.to_string());
                    tiles.push(',');
                    tiles.push_str(&y.to_string());
                    tiles.push(',');
                    tiles.push_str(&h.to_string());
                    tiles.push(']');
                    count += 1;
                }
            }
        }
    }
    tiles.push(']');
    if let Ok(mut guard) = OBSTACLE_TILE_CACHE.lock() {
        *guard = Some((map_num, tiles.clone()));
    }
    out.push_str(&tiles);
}

fn append_party(out: &mut String, engine: &Engine) {
    let g = &engine.globals;
    let roles = &g.game.player_roles;
    let n = (g.max_party_member_index as usize + 1).min(MAX_PLAYABLE_PLAYER_ROLES);
    out.push('[');
    for i in 0..n {
        if i > 0 {
            out.push(',');
        }
        let role = (g.party[i].player_role as usize).min(MAX_PLAYER_ROLES.saturating_sub(1));
        let name_idx = roles.name[role] as usize;
        let level = roles.level[role] as usize;
        let exp = g.exp.primary_exp[role].exp as u64;
        let next_exp = if level < g.game.level_up_exp.len() && level <= MAX_LEVELS {
            g.game.level_up_exp[level] as u64
        } else {
            0
        };
        out.push('{');
        push_u64(out, "slot", i as u64);
        out.push(',');
        push_u64(out, "role", role as u64);
        out.push(',');
        push_str(out, "name", &word_utf8(engine, name_idx));
        out.push(',');
        push_u64(out, "name_id", name_idx as u64);
        out.push(',');
        push_u64(out, "level", roles.level[role] as u64);
        out.push(',');
        push_u64(out, "hp", roles.hp[role] as u64);
        out.push(',');
        push_u64(out, "max_hp", roles.max_hp[role] as u64);
        out.push(',');
        push_u64(out, "mp", roles.mp[role] as u64);
        out.push(',');
        push_u64(out, "max_mp", roles.max_mp[role] as u64);
        out.push(',');
        push_u64(out, "exp", exp);
        out.push(',');
        push_u64(out, "next_exp", next_exp);
        out.push(',');
        push_u64(out, "attack", roles.attack_strength[role] as u64);
        out.push(',');
        push_u64(out, "magic_attack", roles.magic_strength[role] as u64);
        out.push(',');
        push_u64(out, "defense", roles.defense[role] as u64);
        out.push(',');
        push_u64(out, "dexterity", roles.dexterity[role] as u64);
        out.push(',');
        push_u64(out, "flee_rate", roles.flee_rate[role] as u64);
        out.push(',');
        // Screen-space sprite origin (not world coords — use top-level `player`).
        push_pair(out, "screen_pos", g.party[i].x as i32, g.party[i].y as i32);
        out.push(',');
        out.push_str("\"equipment\":[");
        let mut first_e = true;
        for e in 0..MAX_PLAYER_EQUIPMENTS {
            let item = roles.equipment[e][role];
            if item == 0 {
                continue;
            }
            if !first_e {
                out.push(',');
            }
            first_e = false;
            out.push('{');
            push_u64(out, "slot", e as u64);
            out.push(',');
            push_u64(out, "item", item as u64);
            out.push(',');
            push_str(out, "name", &word_utf8(engine, item as usize));
            out.push('}');
        }
        out.push_str("],");
        // Magics with cost / target / affordability.
        out.push_str("\"magics\":[");
        let mut first_m = true;
        let player_mp = roles.mp[role];
        for m in 0..MAX_PLAYER_MAGICS {
            let mid = roles.magic[m][role];
            if mid == 0 {
                continue;
            }
            if !first_m {
                out.push(',');
            }
            first_m = false;
            let (mp_cost, tgt, all, in_battle_ok, out_battle_ok) = magic_meta(engine, mid);
            out.push('{');
            push_u64(out, "id", mid as u64);
            out.push(',');
            push_str(out, "name", &word_utf8(engine, mid as usize));
            out.push(',');
            push_u64(out, "mp", mp_cost as u64);
            out.push(',');
            push_str(out, "tgt", tgt);
            if all {
                out.push(',');
                push_bool(out, "all", true);
            }
            if mp_cost > player_mp {
                out.push(',');
                push_bool(out, "ok", false);
            }
            // Compact battle/field usability when restricted.
            if !in_battle_ok {
                out.push(',');
                push_bool(out, "battle", false);
            }
            if !out_battle_ok {
                out.push(',');
                push_bool(out, "field", false);
            }
            out.push('}');
        }
        out.push(']');
        // Status: only non-zero timers, with short names.
        let mut first_s = true;
        for s in 0..STATUS_ALL {
            let v = g.player_status[role][s];
            if v == 0 {
                continue;
            }
            if first_s {
                out.push(',');
                out.push_str("\"status\":[");
                first_s = false;
            } else {
                out.push(',');
            }
            out.push('{');
            push_u64(out, "id", s as u64);
            out.push(',');
            push_str(out, "name", STATUS_NAMES[s]);
            out.push(',');
            push_u64(out, "t", v as u64);
            out.push('}');
        }
        if !first_s {
            out.push(']');
        }
        out.push('}');
    }
    out.push(']');
}

fn magic_meta(engine: &Engine, magic_obj: u16) -> (u16, &'static str, bool, bool, bool) {
    let obj = engine
        .globals
        .game
        .objects
        .get(magic_obj as usize);
    let Some(obj) = obj else {
        return (0, "ally", false, true, true);
    };
    let magic_num = obj.magic_number() as usize;
    let cost = engine
        .globals
        .game
        .magics
        .get(magic_num)
        .map(|m| m.cost_mp)
        .unwrap_or(0);
    let flags = obj.magic_flags();
    let to_enemy = flags & MAGICFLAG_USABLE_TO_ENEMY != 0;
    let all = flags & MAGICFLAG_APPLY_TO_ALL != 0;
    let in_battle = flags & MAGICFLAG_USABLE_IN_BATTLE != 0;
    let out_battle = flags & MAGICFLAG_USABLE_OUTSIDE_BATTLE != 0;
    let tgt = if to_enemy { "enemy" } else { "ally" };
    (cost, tgt, all, in_battle, out_battle)
}

fn append_inventory(out: &mut String, engine: &Engine) {
    out.push('[');
    let mut listed = 0usize;
    let mut first = true;
    for inv in engine.globals.inventory.iter().take(MAX_INVENTORY) {
        if inv.item == 0 || inv.amount == 0 {
            continue;
        }
        if listed >= MAX_INV_LIST {
            break;
        }
        listed += 1;
        if !first {
            out.push(',');
        }
        first = false;
        let flags = engine
            .globals
            .game
            .objects
            .get(inv.item as usize)
            .map(|o| o.item_flags())
            .unwrap_or(0);
        out.push('{');
        push_u64(out, "item", inv.item as u64);
        out.push(',');
        push_str(out, "name", &word_utf8(engine, inv.item as usize));
        out.push(',');
        push_u64(out, "amount", inv.amount as u64);
        let mut tags: Vec<&str> = Vec::new();
        if flags & ITEMFLAG_USABLE != 0 {
            tags.push("use");
        }
        if flags & ITEMFLAG_EQUIPABLE != 0 {
            tags.push("eq");
        }
        if flags & ITEMFLAG_THROWABLE != 0 {
            tags.push("throw");
        }
        if flags & ITEMFLAG_CONSUMING != 0 {
            tags.push("consume");
        }
        if flags & ITEMFLAG_APPLY_TO_ALL != 0 {
            tags.push("all");
        }
        if flags & ITEMFLAG_SELLABLE != 0 {
            tags.push("sell");
        }
        if !tags.is_empty() {
            out.push(',');
            out.push_str("\"tags\":[");
            for (ti, t) in tags.iter().enumerate() {
                if ti > 0 {
                    out.push(',');
                }
                push_json_string(out, t);
            }
            out.push(']');
        }
        out.push('}');
    }
    out.push(']');
}

// ---------------------------------------------------------------------------
// Events (observation only — no recommended path / target)
// ---------------------------------------------------------------------------

/// How a trigger script looks when scanned (label for AI; not a goal ranking).
/// Labels: item / quest / scene / dialog / battle / cash / mild / none.
#[derive(Clone, Copy)]
struct ScriptRank {
    rank: u8,
    label: &'static str,
    grants_item: Option<u16>,
}

fn compute_walk(engine: &Engine, player: (i32, i32)) -> [bool; 4] {
    let mut walk = [false; 4];
    for (i, &((dx, dy), _)) in WALK_DELTA.iter().enumerate() {
        let blocked =
            engine.check_obstacle_with_range((player.0 + dx, player.1 + dy), true, 0, true);
        walk[i] = !blocked;
    }
    walk
}

/// Nearby event objects as pure observation for the agent.
fn build_events_json(engine: &Engine, player: (i32, i32)) -> String {
    let g = &engine.globals;
    let scene_i = g.num_scene as usize;
    if scene_i == 0 || scene_i > g.game.scenes.len() {
        return "[]".into();
    }
    let start = g.game.scenes[scene_i - 1].event_object_index as usize;
    let end = g
        .game
        .scenes
        .get(scene_i)
        .map(|s| s.event_object_index as usize)
        .unwrap_or(g.game.event_objects.len());
    let party_dir = g.party_direction;

    struct Row {
        dist: i32,
        event_id: u16,
        index: usize,
        role: &'static str,
        kind: &'static str,
        search_ok: bool,
        face: Option<&'static str>,
        in_touch: bool,
        dest_scene: Option<u16>,
        progress: &'static str,
        item_use: Option<u16>,
        dialog_loop: bool,
    }

    let mut rows: Vec<Row> = Vec::new();
    for index in start..end.min(g.game.event_objects.len()) {
        let ev = g.game.event_objects[index];
        if ev.state <= 0 || ev.vanish_time != 0 {
            continue;
        }
        let event_id = (index + 1) as u16;
        let pos = (ev.x as i32, ev.y as i32);
        let dist = metric(player, pos);
        if dist > 400 && ev.trigger_script == 0 && ev.auto_script == 0 {
            continue;
        }
        let kind = if ev.trigger_mode >= 4 {
            "touch"
        } else if ev.trigger_mode == 0 {
            "scenery"
        } else {
            "search"
        };
        let dest_scene = script_destination_scene(engine, ev.trigger_script);
        let role = classify_event_role(ev.trigger_mode, ev.sprite_num, ev.trigger_script, dest_scene);
        let (search_ok, face) = if ev.trigger_mode > 0 && ev.trigger_mode < 4 {
            search_status(player, pos, ev.trigger_mode, party_dir)
        } else {
            (false, None)
        };
        let touch_radius = if ev.trigger_mode >= 4 {
            ((ev.trigger_mode - 4) as i32 * 32 + 16).max(16)
        } else {
            0
        };
        let in_touch = touch_radius > 0 && dist < touch_radius;
        let item_use = story_item_for_event(engine, event_id);
        let mut rank = analyze_script_progress(engine, ev.trigger_script);
        if item_use.is_some() {
            rank = ScriptRank {
                rank: 0,
                label: "item",
                grants_item: item_use,
            };
        } else if dest_scene.is_some() && rank.rank > 3 {
            rank = ScriptRank {
                rank: 3,
                label: "scene",
                grants_item: None,
            };
        }
        rows.push(Row {
            dist,
            event_id,
            index,
            role,
            kind,
            search_ok,
            face,
            in_touch,
            dest_scene,
            progress: rank.label,
            item_use,
            dialog_loop: rank.rank >= 5,
        });
    }
    rows.sort_by_key(|r| (r.dist, r.event_id));
    rows.truncate(MAX_EVENTS);

    let mut events = String::from("[");
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            events.push(',');
        }
        let ev = g.game.event_objects[row.index];
        let pos = (ev.x as i32, ev.y as i32);
        events.push('{');
        push_u64(&mut events, "id", row.event_id as u64);
        events.push(',');
        push_str(&mut events, "kind", row.kind);
        events.push(',');
        push_str(&mut events, "role", row.role);
        events.push(',');
        push_pair(&mut events, "pos", pos.0, pos.1);
        events.push(',');
        push_pair(&mut events, "delta", pos.0 - player.0, pos.1 - player.1);
        events.push(',');
        push_i64(&mut events, "dist", row.dist as i64);
        events.push(',');
        push_i64(&mut events, "state", ev.state as i64);
        events.push(',');
        push_u64(&mut events, "trigger_mode", ev.trigger_mode as u64);
        events.push(',');
        push_u64(&mut events, "trigger_script", ev.trigger_script as u64);
        events.push(',');
        push_u64(&mut events, "auto_script", ev.auto_script as u64);
        events.push(',');
        push_u64(&mut events, "sprite_num", ev.sprite_num as u64);
        events.push(',');
        push_u64(&mut events, "direction", ev.direction as u64);
        events.push(',');
        push_bool(&mut events, "can_search_now", row.search_ok);
        events.push(',');
        push_bool(&mut events, "in_touch_range", row.in_touch);
        events.push(',');
        push_str(&mut events, "progress", row.progress);
        if row.dialog_loop {
            events.push(',');
            push_bool(&mut events, "loop", true);
        }
        if let Some(item) = row.item_use {
            events.push(',');
            push_u64(&mut events, "item_use", item as u64);
        }
        if let Some(f) = row.face {
            events.push(',');
            push_str(&mut events, "face", f);
            events.push(',');
            push_bool(&mut events, "in_search_range", true);
        }
        if let Some(ds) = row.dest_scene {
            events.push(',');
            push_u64(&mut events, "dest_scene", ds as u64);
        }
        events.push('}');
    }
    events.push(']');
    events
}

fn classify_event_role(
    trigger_mode: u16,
    sprite_num: u16,
    trigger_script: u16,
    dest_scene: Option<u16>,
) -> &'static str {
    if trigger_script == 0 {
        return "decor";
    }
    if dest_scene.is_some() {
        return "exit";
    }
    if trigger_mode >= 4 {
        if sprite_num == 0 {
            "exit"
        } else {
            "npc"
        }
    } else if trigger_mode == 0 {
        "decor"
    } else if sprite_num == 0 {
        "trigger"
    } else {
        "npc"
    }
}

fn dir_to_key(dir: u16) -> &'static str {
    DIR_KEYS[(dir as usize) % 4]
}

fn metric(a: (i32, i32), b: (i32, i32)) -> i32 {
    (a.0 - b.0).abs() + (a.1 - b.1).abs() * 2
}

/// Map position → (tile_x, tile_y, half) matching `play::search`.
fn tile_of(pos: (i32, i32)) -> (i32, i32, i32) {
    (
        pos.0 / 32,
        pos.1 / 16,
        if pos.0 % 32 != 0 { 1 } else { 0 },
    )
}

/// Search-cone offsets for a party facing (DIR_SOUTH/WEST/NORTH/EAST).
fn dir_step_offsets(direction: u16) -> (i32, i32) {
    // Matches `play::get_search_trigger_range`.
    let x_offset = if direction == 2 || direction == 3 {
        // NORTH or EAST
        16
    } else {
        -16
    };
    let y_offset = if direction == 3 || direction == 0 {
        // EAST or SOUTH
        8
    } else {
        -8
    };
    (x_offset, y_offset)
}

fn search_range(position: (i32, i32), direction: u16) -> [(i32, i32); 13] {
    let (x_offset, y_offset) = dir_step_offsets(direction);
    let mut x = position.0;
    let mut y = position.1;
    let mut range = [(0i32, 0i32); 13];
    range[0] = position;
    for i in 0..4 {
        range[i * 3 + 1] = (x + x_offset, y + y_offset);
        range[i * 3 + 2] = (x, y + y_offset * 2);
        range[i * 3 + 3] = (x + 2 * x_offset, y);
        x += x_offset;
        y += y_offset;
    }
    range
}

/// Engine-accurate search check (tile match + facing cone).
/// Returns `(can_search_now, face_key_if_any_dir_works)`.
fn search_status(
    player: (i32, i32),
    event: (i32, i32),
    mode: u16,
    party_dir: u16,
) -> (bool, Option<&'static str>) {
    if mode == 0 || mode >= 4 {
        return (false, None);
    }
    let et = tile_of(event);
    let mut face: Option<&'static str> = None;
    let mut now = false;
    // Prefer current facing first so `face` matches party when both work.
    let order = [
        party_dir % 4,
        (party_dir + 1) % 4,
        (party_dir + 2) % 4,
        (party_dir + 3) % 4,
    ];
    for d in order {
        let range = search_range(player, d);
        for (i, p) in range.iter().enumerate() {
            // `play::search`: skip when (mode * 6 - 4) <= i
            if (mode as i32) * 6 - 4 <= i as i32 {
                continue;
            }
            if tile_of(*p) == et {
                if face.is_none() {
                    face = Some(dir_to_key(d));
                }
                if d == party_dir % 4 {
                    now = true;
                }
                break;
            }
        }
    }
    (now, face)
}

fn script_destination_scene(engine: &Engine, script: u16) -> Option<u16> {
    if script == 0 {
        return None;
    }
    let start = script as usize;
    for index in start..start.saturating_add(24) {
        let entry = engine.globals.game.script_entries.get(index)?;
        // 0x0059 = teleport / change scene.
        if entry.operation == 0x0059 && entry.operand[0] != 0 {
            return Some(entry.operand[0]);
        }
        if entry.operation == 0x0000 {
            break;
        }
    }
    None
}

/// When the best progress target is walk-unreachable, pick a **reachable door**
/// that leads to a scene which has a portal landing near that target (one hop
/// out and back). Example: kitchen → door #13 → scene 3 → door #52 → hall #16.

fn analyze_script_progress(engine: &Engine, script: u16) -> ScriptRank {
    analyze_script_progress_depth(engine, script, 0)
}

fn analyze_script_progress_depth(engine: &Engine, script: u16, depth: u8) -> ScriptRank {
    if script == 0 {
        return ScriptRank {
            rank: 6,
            label: "none",
            grants_item: None,
        };
    }
    let entries = &engine.globals.game.script_entries;
    let mut grants_item: Option<u16> = None;
    let mut has_item = false;
    let mut has_cash = false;
    let mut has_battle = false;
    let mut has_scene = false;
    let mut mutates_world = false;
    let mut mild = false;
    let mut saw_dialog = false;
    let mut ip = script as usize;
    let mut steps = 0usize;
    while steps < 96 {
        steps += 1;
        let Some(entry) = entries.get(ip) else {
            break;
        };
        let op = entry.operation;
        let a = entry.operand;
        match op {
            // Hard stop for this interaction (next press starts at next_script).
            0x0000 | 0x0001 => break,
            // Stop & jump entry — end of this run.
            0x0002 => break,
            // Unconditional jump (follow once).
            0x0003 => {
                if a[0] != 0 {
                    ip = a[0] as usize;
                    continue;
                }
            }
            // Call — peek callee briefly for side effects.
            0x0004 => {
                if a[0] != 0 && depth < 2 {
                    let sub = analyze_script_progress_depth(engine, a[0], depth + 1);
                    if sub.rank <= 1 {
                        return sub;
                    }
                    if sub.rank <= 2 {
                        mutates_world = true;
                    }
                    if sub.grants_item.is_some() {
                        grants_item = sub.grants_item;
                        has_item = true;
                    }
                }
            }
            0x0006 => has_battle = true, // start battle
            0x001E => has_cash = true,
            0x001F => {
                has_item = true;
                if a[0] != 0 {
                    grants_item = Some(a[0]);
                }
            }
            0x0020 => has_item = true, // remove item
            // Set trigger/auto/mode/state on objects — quest progression.
            0x0024 | 0x0025 | 0x0040 | 0x0049 => mutates_world = true,
            0x0059 if a[0] != 0 => has_scene = true,
            // Dialog ops.
            0x003B | 0x003C | 0x003D | 0x003E | 0xFFFF => saw_dialog = true,
            // Movement / wait / redraw — mild.
            0x0005 | 0x0008 | 0x0009 | 0x000B..=0x0016 | 0x006C | 0x006E | 0x0070 => {
                mild = true;
            }
            _ => {
                // Unknown opcode: treat as mild progress so we don't skip it.
                mild = true;
            }
        }
        ip = ip.saturating_add(1);
    }

    if has_item || has_cash || has_battle {
        ScriptRank {
            rank: 1,
            label: if has_item {
                "item"
            } else if has_battle {
                "battle"
            } else {
                "cash"
            },
            grants_item,
        }
    } else if mutates_world {
        ScriptRank {
            rank: 2,
            label: "quest",
            grants_item: None,
        }
    } else if has_scene {
        ScriptRank {
            rank: 3,
            label: "scene",
            grants_item: None,
        }
    } else if mild && !saw_dialog {
        ScriptRank {
            rank: 4,
            label: "mild",
            grants_item: None,
        }
    } else if saw_dialog || mild {
        ScriptRank {
            rank: 5,
            label: "dialog",
            grants_item: None,
        }
    } else {
        ScriptRank {
            rank: 6,
            label: "none",
            grants_item: None,
        }
    }
}

/// Inventory item whose use-script checks event via opcode 0x0081.
fn story_item_for_event(engine: &Engine, event_id: u16) -> Option<u16> {
    for entry in engine.globals.inventory.iter() {
        if entry.item == 0 || entry.amount == 0 {
            continue;
        }
        let Some(object) = engine.globals.game.objects.get(entry.item as usize) else {
            continue;
        };
        let flags = object.item_flags();
        if flags & ITEMFLAG_USABLE == 0 {
            continue;
        }
        let use_script = object.item_script_on_use();
        if use_script == 0 {
            continue;
        }
        // Item use scripts often start with a redraw (0x0005) then 0x0081.
        let start = use_script as usize;
        for index in start..start.saturating_add(8) {
            let Some(e) = engine.globals.game.script_entries.get(index) else {
                break;
            };
            if e.operation == 0x0081 && e.operand[0] == event_id {
                return Some(entry.item);
            }
            if e.operation == 0x0000 {
                break;
            }
        }
    }
    None
}

/// BFS short path; returns key names (up/right/down/left), truncated.
/// Prefer expanding current facing first to reduce path flip-flop.


// ---------------------------------------------------------------------------
// Battle
// ---------------------------------------------------------------------------

fn append_battle(out: &mut String, engine: &Engine, battle: &crate::battle::Battle) {
    out.push('{');
    push_str(out, "phase", battle_phase_name(battle.phase));
    out.push(',');
    push_str(out, "ui_state", battle_ui_state_name(battle.ui.state));
    out.push(',');
    push_str(out, "menu_state", battle_menu_state_name(battle.ui.menu_state));
    out.push(',');
    push_u64(out, "cur_player_index", battle.ui.cur_player_index as u64);
    out.push(',');
    push_u64(out, "selected_action", battle.ui.selected_action as u64);
    out.push(',');
    push_i64(out, "selected_index", battle.ui.selected_index as i64);
    out.push(',');
    push_bool(out, "auto_attack", battle.ui.auto_attack);
    out.push(',');
    push_bool(out, "force", battle.force);
    out.push(',');
    push_bool(out, "flee", battle.flee);
    out.push(',');
    push_bool(out, "is_boss", battle.is_boss);
    out.push(',');
    push_bool(out, "enemy_cleared", battle.enemy_cleared);
    out.push(',');
    push_str(out, "result", battle_result_name(battle.battle_result));
    out.push(',');
    push_i64(out, "exp_gained", battle.exp_gained as i64);
    out.push(',');
    push_i64(out, "cash_gained", battle.cash_gained as i64);
    out.push(',');
    push_u64(out, "max_enemy_index", battle.max_enemy_index as u64);
    out.push(',');
    push_str(out, "msg", &bytes_to_display(&battle.ui.msg));

    // Explicit target when selecting.
    let selecting_enemy = matches!(
        battle.ui.state,
        BattleUiState::SelectTargetEnemy | BattleUiState::SelectTargetEnemyAll
    );
    let selecting_player = matches!(
        battle.ui.state,
        BattleUiState::SelectTargetPlayer | BattleUiState::SelectTargetPlayerAll
    );
    if selecting_enemy || selecting_player {
        out.push(',');
        out.push_str("\"target\":{");
        push_str(
            out,
            "side",
            if selecting_enemy { "enemy" } else { "player" },
        );
        out.push(',');
        push_i64(out, "index", battle.ui.selected_index as i64);
        out.push(',');
        push_bool(
            out,
            "all",
            matches!(
                battle.ui.state,
                BattleUiState::SelectTargetEnemyAll | BattleUiState::SelectTargetPlayerAll
            ),
        );
        out.push('}');
    }

    out.push(',');
    out.push_str("\"enemies\":[");
    let mut first = true;
    for i in 0..=battle.max_enemy_index as usize {
        if i >= MAX_ENEMIES_IN_TEAM {
            break;
        }
        let e = &battle.enemy[i];
        if e.object_id == 0 {
            continue;
        }
        if !first {
            out.push(',');
        }
        first = false;
        let enemy_id = engine
            .globals
            .game
            .objects
            .get(e.object_id as usize)
            .map(|o| o.enemy_id() as usize)
            .unwrap_or(0);
        let template_hp = engine
            .globals
            .game
            .enemies
            .get(enemy_id)
            .map(|t| t.health)
            .unwrap_or(e.e.health);
        let max_hp = template_hp.max(e.e.health).max(e.prev_hp);
        let selected = selecting_enemy && battle.ui.selected_index as usize == i;
        out.push('{');
        push_u64(out, "index", i as u64);
        out.push(',');
        push_u64(out, "object_id", e.object_id as u64);
        out.push(',');
        push_str(out, "name", &word_utf8(engine, e.object_id as usize));
        out.push(',');
        push_u64(out, "hp", e.e.health as u64);
        out.push(',');
        push_u64(out, "max_hp", max_hp as u64);
        out.push(',');
        push_u64(out, "level", e.e.level as u64);
        out.push(',');
        push_u64(out, "attack", e.e.attack_strength as u64);
        out.push(',');
        push_u64(out, "defense", e.e.defense as u64);
        out.push(',');
        push_str(out, "state", fighter_state_name(e.state));
        out.push(',');
        push_f64(out, "time_meter", f64::from(e.time_meter));
        out.push(',');
        push_pair(out, "pos", e.pos.0, e.pos.1);
        if selected {
            out.push(',');
            push_bool(out, "selected", true);
        }
        // Enemy status timers.
        let mut first_s = true;
        for s in 0..STATUS_ALL {
            let v = e.status[s];
            if v == 0 {
                continue;
            }
            if first_s {
                out.push(',');
                out.push_str("\"status\":[");
                first_s = false;
            } else {
                out.push(',');
            }
            out.push('{');
            push_u64(out, "id", s as u64);
            out.push(',');
            push_str(out, "name", STATUS_NAMES[s]);
            out.push(',');
            push_u64(out, "t", v as u64);
            out.push('}');
        }
        if !first_s {
            out.push(']');
        }
        out.push('}');
    }
    out.push_str("],\"players\":[");
    first = true;
    let n = (engine.globals.max_party_member_index as usize + 1).min(MAX_PLAYERS_IN_PARTY);
    for i in 0..n {
        if !first {
            out.push(',');
        }
        first = false;
        let p = &battle.player[i];
        let role = engine.globals.party[i].player_role as usize;
        let roles = &engine.globals.game.player_roles;
        let name_idx = roles.name[role] as usize;
        let selected = selecting_player && battle.ui.selected_index as usize == i;
        let is_acting = battle.ui.cur_player_index as usize == i
            && battle.phase == BattlePhase::SelectAction;
        out.push('{');
        push_u64(out, "slot", i as u64);
        out.push(',');
        push_u64(out, "role", role as u64);
        out.push(',');
        push_str(out, "name", &word_utf8(engine, name_idx));
        out.push(',');
        push_u64(out, "hp", roles.hp[role] as u64);
        out.push(',');
        push_u64(out, "max_hp", roles.max_hp[role] as u64);
        out.push(',');
        push_u64(out, "mp", roles.mp[role] as u64);
        out.push(',');
        push_u64(out, "max_mp", roles.max_mp[role] as u64);
        out.push(',');
        push_str(out, "state", fighter_state_name(p.state));
        out.push(',');
        push_f64(out, "time_meter", f64::from(p.time_meter));
        out.push(',');
        push_bool(out, "defending", p.defending);
        if selected {
            out.push(',');
            push_bool(out, "selected", true);
        }
        if is_acting {
            out.push(',');
            push_bool(out, "acting", true);
        }
        // Player battle status from globals.
        let mut first_s = true;
        for s in 0..STATUS_ALL {
            let v = engine.globals.player_status[role][s];
            if v == 0 {
                continue;
            }
            if first_s {
                out.push(',');
                out.push_str("\"status\":[");
                first_s = false;
            } else {
                out.push(',');
            }
            out.push('{');
            push_u64(out, "id", s as u64);
            out.push(',');
            push_str(out, "name", STATUS_NAMES[s]);
            out.push(',');
            push_u64(out, "t", v as u64);
            out.push('}');
        }
        if !first_s {
            out.push(']');
        }
        out.push('}');
    }
    out.push(']');
    out.push('}');
}

// ---------------------------------------------------------------------------
// Hints
// ---------------------------------------------------------------------------



// ---------------------------------------------------------------------------
// Name helpers
// ---------------------------------------------------------------------------

fn battle_phase_name(p: BattlePhase) -> &'static str {
    match p {
        BattlePhase::SelectAction => "select_action",
        BattlePhase::PerformAction => "perform_action",
    }
}

fn battle_ui_state_name(s: BattleUiState) -> &'static str {
    match s {
        BattleUiState::Wait => "wait",
        BattleUiState::SelectMove => "select_move",
        BattleUiState::SelectTargetEnemy => "select_target_enemy",
        BattleUiState::SelectTargetPlayer => "select_target_player",
        BattleUiState::SelectTargetEnemyAll => "select_target_enemy_all",
        BattleUiState::SelectTargetPlayerAll => "select_target_player_all",
    }
}

fn battle_menu_state_name(s: BattleMenuState) -> &'static str {
    match s {
        BattleMenuState::Main => "main",
        BattleMenuState::MagicSelect => "magic_select",
        BattleMenuState::UseItemSelect => "use_item_select",
        BattleMenuState::ThrowItemSelect => "throw_item_select",
        BattleMenuState::Misc => "misc",
        BattleMenuState::MiscItemSubMenu => "misc_item_sub",
    }
}

fn battle_result_name(r: crate::battle::BattleResult) -> &'static str {
    use crate::battle::BattleResult;
    match r {
        BattleResult::Won => "won",
        BattleResult::Lost => "lost",
        BattleResult::Fleed => "fleed",
        BattleResult::Terminated => "terminated",
        BattleResult::OnGoing => "ongoing",
        BattleResult::PreBattle => "pre_battle",
        BattleResult::Pause => "pause",
    }
}

fn fighter_state_name(s: FighterState) -> &'static str {
    match s {
        FighterState::Wait => "wait",
        FighterState::Com => "com",
        FighterState::Act => "act",
    }
}

fn word_utf8(engine: &Engine, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    agent_text_from_bytes(&engine.texts.word(n))
}

fn bytes_to_display(bytes: &[u8]) -> String {
    agent_text_from_bytes(bytes)
}

// --- minimal JSON helpers (no serde) ---

fn push_str(out: &mut String, key: &str, val: &str) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    push_json_string(out, val);
}

fn push_json_string(out: &mut String, val: &str) {
    out.push('"');
    for c in val.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_bool(out: &mut String, key: &str, val: bool) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push_str(if val { "true" } else { "false" });
}

fn push_u64(out: &mut String, key: &str, val: u64) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push_str(&val.to_string());
}

fn push_i64(out: &mut String, key: &str, val: i64) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push_str(&val.to_string());
}

fn push_f64(out: &mut String, key: &str, val: f64) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    if val.is_finite() {
        out.push_str(&format!("{val:.3}"));
    } else {
        out.push_str("0");
    }
}

fn push_pair(out: &mut String, key: &str, a: i32, b: i32) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":[");
    out.push_str(&a.to_string());
    out.push(',');
    out.push_str(&b.to_string());
    out.push(']');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_loop::Engine;

    fn engine() -> Engine {
        std::env::set_var("PAL_DATA_DIR", concat!(env!("CARGO_MANIFEST_DIR"), "/pal"));
        Engine::new(true).expect("headless engine")
    }

    #[test]
    fn script_rank_dialog_loop_vs_item_delivery() {
        let e = engine();
        let aunt = analyze_script_progress(&e, 4981);
        assert_eq!(aunt.label, "dialog");
        let stairs = analyze_script_progress(&e, 4885);
        assert_eq!(stairs.label, "item");
        assert_eq!(stairs.grants_item, Some(272));
    }

    #[test]
    fn state_is_observation_not_strategy() {
        let mut e = engine();
        e.globals.num_scene = 1;
        e.globals.in_main_game = true;
        e.globals.load_flags |= crate::global::LOAD_SCENE | crate::global::LOAD_PLAYER_SPRITE;
        e.load_resources();
        e.globals.viewport = (608, 1024);
        e.globals.partyoffset = (160, 112);
        let json = build_state_json(&e);
        assert!(!json.contains("\"nav\""), "nav must not be published");
        assert!(!json.contains("\"keys_hint\""));
        assert!(!json.contains("\"hint\""));
        // Party/inventory moved to separate endpoints.
        assert!(!json.contains("\"party\":[") && !json.contains("\"party\": ["));
        assert!(!json.contains("\"inventory\":[") && !json.contains("\"inventory\": ["));
        assert!(json.contains("\"resources\""));
        assert!(json.contains("/v1/party"));
        assert!(json.contains("\"map\":"));
        assert!(json.contains("\"dirs\""));
        assert!(json.contains("\"exits\""));
        assert!(json.contains("\"mechanisms\""));
        assert!(json.contains("\"obstacles\""));
        assert!(json.contains("\"events\":"));
        assert!(json.contains("\"player\":"));
        assert!(json.contains("\"walk\":"));
    }

    #[test]
    fn party_and_inventory_endpoints_json() {
        let mut e = engine();
        e.globals.in_main_game = true;
        e.globals.add_item_to_inventory(92, 1);
        let p = build_party_json(&e);
        assert!(p.contains("\"party\":"));
        assert!(p.contains("李逍遙") || p.contains("\"name\""));
        let inv = build_inventory_json(&e);
        assert!(inv.contains("\"inventory\":"));
        assert!(inv.contains("\"item\":92") || inv.contains("水果"));
    }

    #[test]
    fn map_dirs_match_walk_deltas() {
        let mut e = engine();
        e.globals.num_scene = 1;
        e.globals.in_main_game = true;
        e.globals.load_flags |= crate::global::LOAD_SCENE | crate::global::LOAD_PLAYER_SPRITE;
        e.load_resources();
        let json = build_state_json(&e);
        assert!(json.contains("\"up\":{\"dx\":16,\"dy\":-8"));
        assert!(json.contains("\"right\":{\"dx\":16,\"dy\":8"));
        assert!(json.contains("\"down\":{\"dx\":-16,\"dy\":8"));
        assert!(json.contains("\"left\":{\"dx\":-16,\"dy\":-8"));
    }

    #[test]
    fn osmanthus_wine_targets_drunkard() {
        let mut e = engine();
        e.globals.add_item_to_inventory(272, 1);
        assert_eq!(story_item_for_event(&e, 63), Some(272));
        assert_eq!(story_item_for_event(&e, 20), None);
    }
}

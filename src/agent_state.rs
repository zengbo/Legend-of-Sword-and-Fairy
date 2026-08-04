//! Rich JSON snapshot for `GET /v1/state` (AI / HTTP driver).
//!
//! Kept out of `game_loop.rs` so the observe surface can grow without
//! cluttering the engine core.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::battle::{BattleMenuState, BattlePhase, BattleUiState, FighterState};
use crate::game_loop::Engine;
use crate::global::{
    ITEMFLAG_APPLY_TO_ALL, ITEMFLAG_CONSUMING, ITEMFLAG_EQUIPABLE, ITEMFLAG_SELLABLE,
    ITEMFLAG_THROWABLE, ITEMFLAG_USABLE, MAGICFLAG_APPLY_TO_ALL, MAGICFLAG_USABLE_IN_BATTLE,
    MAGICFLAG_USABLE_OUTSIDE_BATTLE, MAGICFLAG_USABLE_TO_ENEMY, MAX_ENEMIES_IN_TEAM,
    MAX_INVENTORY, MAX_LEVELS, MAX_PLAYABLE_PLAYER_ROLES, MAX_PLAYER_EQUIPMENTS, MAX_PLAYER_MAGICS,
    MAX_PLAYER_ROLES, MAX_PLAYERS_IN_PARTY, STATUS_ALL,
};
use crate::ui::{agent_text_from_bytes, AgentMenuItem};
use crate::ui_driver;

/// Overworld step deltas matching `play` / `fullgame_autoplay` key mapping:
/// up / right / down / left.
const WALK_DELTA: [((i32, i32), &str); 4] = [
    ((16, -8), "up"),
    ((16, 8), "right"),
    ((-16, 8), "down"),
    ((-16, -8), "left"),
];

/// Max nearby event objects listed (sorted by distance).
const MAX_EVENTS: usize = 48;
/// Max inventory rows listed.
const MAX_INV_LIST: usize = 64;
/// BFS node budget for short path hints (per state build).
const PATH_BFS_LIMIT: usize = 6_000;
/// Max path steps published in `nav.path`.
const PATH_MAX_STEPS: usize = 16;
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

    // Four-way walkability.
    out.push(',');
    out.push_str("\"walk\":{");
    for (i, &((_, _), name)) in WALK_DELTA.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_bool(&mut out, name, walk[i]);
    }
    out.push('}');

    // Party (rich).
    out.push(',');
    out.push_str("\"party\":");
    append_party(&mut out, engine);

    // Inventory.
    out.push(',');
    out.push_str("\"inventory\":");
    append_inventory(&mut out, engine);

    // Nearby / active event objects + nav + path for best target.
    let (events_json, nav, path_keys) = build_events_and_nav(engine, player, &walk);
    out.push(',');
    out.push_str("\"events\":");
    out.push_str(&events_json);

    out.push(',');
    out.push_str("\"nav\":");
    append_nav(&mut out, &nav, &path_keys);

    // Battle block (null when not in battle).
    out.push(',');
    out.push_str("\"battle\":");
    if let Some(battle) = engine.battle.as_ref() {
        append_battle(&mut out, engine, battle);
    } else {
        out.push_str("null");
    }

    // Natural-language one-liner for agents.
    out.push(',');
    out.push_str("\"hint\":");
    push_json_string(
        &mut out,
        &build_hint(engine, phase, in_dialog, in_battle, in_menu, &nav, &path_keys, &walk),
    );

    // Context-sensitive key suggestions + full legal key vocabulary.
    out.push(',');
    out.push_str("\"keys_hint\":");
    append_keys_hint(&mut out, phase, in_dialog, in_battle, in_menu, &nav, &path_keys);
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
// Events + navigation
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct NavInfo {
    event_id: u16,
    role: &'static str,
    keys: Vec<&'static str>,
    can_act: bool,
    dist: i32,
    dest_scene: Option<u16>,
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

fn build_events_and_nav(
    engine: &Engine,
    player: (i32, i32),
    walk: &[bool; 4],
) -> (String, NavInfo, Vec<&'static str>) {
    let g = &engine.globals;
    let scene_i = g.num_scene as usize;
    if scene_i == 0 || scene_i > g.game.scenes.len() {
        return ("[]".into(), NavInfo::default(), Vec::new());
    }
    let start = g.game.scenes[scene_i - 1].event_object_index as usize;
    let end = g
        .game
        .scenes
        .get(scene_i)
        .map(|s| s.event_object_index as usize)
        .unwrap_or(g.game.event_objects.len());

    struct Row {
        dist: i32,
        event_id: u16,
        index: usize,
        role: &'static str,
        kind: &'static str,
        search_ok: bool,
        in_touch: bool,
        dest_scene: Option<u16>,
        keys: Vec<&'static str>,
        interactable: bool,
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
        let search_ok = if ev.trigger_mode > 0 && ev.trigger_mode < 4 {
            can_search_from(player, pos, ev.trigger_mode)
        } else {
            false
        };
        let touch_radius = if ev.trigger_mode >= 4 {
            ((ev.trigger_mode - 4) as i32 * 32 + 16).max(16)
        } else {
            0
        };
        let in_touch = touch_radius > 0 && dist < touch_radius;
        let interactable = ev.trigger_script != 0
            && (ev.trigger_mode > 0 || dest_scene.is_some());
        let keys = keys_toward(player, pos, walk);
        rows.push(Row {
            dist,
            event_id,
            index,
            role,
            kind,
            search_ok,
            in_touch,
            dest_scene,
            keys,
            interactable,
        });
    }
    rows.sort_by_key(|r| (r.dist, r.event_id));
    rows.truncate(MAX_EVENTS);

    // Choose best nav target: nearest interactable that is not pure scenery.
    let mut nav = NavInfo::default();
    if let Some(best) = rows
        .iter()
        .filter(|r| r.interactable)
        .min_by_key(|r| {
            // Prefer actable now, then nearby, then search/npc over far exits.
            let act_pen = if r.search_ok || r.in_touch { 0 } else { 1 };
            let near = if r.dist <= 320 {
                0
            } else if r.dist <= 640 {
                1
            } else {
                2
            };
            let role_pen = match r.role {
                "npc" | "search" => 0,
                "exit" | "trigger" => 1,
                _ => 2,
            };
            (act_pen, near, role_pen, r.dist, r.event_id)
        })
    {
        nav = NavInfo {
            event_id: best.event_id,
            role: best.role,
            keys: best.keys.clone(),
            can_act: best.search_ok || best.in_touch,
            dist: best.dist,
            dest_scene: best.dest_scene,
        };
    }

    // Short path BFS toward nav target (only when not already actable).
    let mut path_keys: Vec<&'static str> = Vec::new();
    if nav.event_id != 0 && !nav.can_act {
        if let Some(ev) = g
            .game
            .event_objects
            .get(nav.event_id as usize - 1)
            .copied()
        {
            path_keys = path_to_event(engine, player, ev, PATH_BFS_LIMIT, PATH_MAX_STEPS);
            // If BFS found a path, first key(s) override keys_toward for nav.
            if let Some(&first) = path_keys.first() {
                nav.keys = vec![first];
            }
        }
    }

    // Serialize events.
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
        if let Some(ds) = row.dest_scene {
            events.push(',');
            push_u64(&mut events, "dest_scene", ds as u64);
        }
        if !row.keys.is_empty() {
            events.push(',');
            events.push_str("\"keys\":[");
            for (ki, k) in row.keys.iter().enumerate() {
                if ki > 0 {
                    events.push(',');
                }
                push_json_string(&mut events, k);
            }
            events.push(']');
        }
        events.push('}');
    }
    events.push(']');
    (events, nav, path_keys)
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
        // Invisible touch zones are usually doors/transitions.
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
        "search"
    }
}

/// Keys among currently-walkable directions that reduce isometric distance.
fn keys_toward(player: (i32, i32), target: (i32, i32), walk: &[bool; 4]) -> Vec<&'static str> {
    let cur = metric(player, target);
    let mut scored: Vec<(i32, usize, &'static str)> = Vec::new();
    for (i, &((dx, dy), name)) in WALK_DELTA.iter().enumerate() {
        if !walk[i] {
            continue;
        }
        let next = (player.0 + dx, player.1 + dy);
        let d = metric(next, target);
        if d < cur {
            scored.push((d, i, name));
        }
    }
    scored.sort_by_key(|&(d, i, _)| (d, i));
    scored.into_iter().map(|(_, _, n)| n).collect()
}

fn append_nav(out: &mut String, nav: &NavInfo, path: &[&'static str]) {
    if nav.event_id == 0 {
        out.push_str("null");
        return;
    }
    out.push('{');
    push_u64(out, "event", nav.event_id as u64);
    out.push(',');
    push_str(out, "role", nav.role);
    out.push(',');
    push_i64(out, "dist", nav.dist as i64);
    out.push(',');
    push_bool(out, "can_act", nav.can_act);
    if let Some(ds) = nav.dest_scene {
        out.push(',');
        push_u64(out, "dest_scene", ds as u64);
    }
    if !nav.keys.is_empty() {
        out.push(',');
        out.push_str("\"keys\":[");
        for (i, k) in nav.keys.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            push_json_string(out, k);
        }
        out.push(']');
    }
    if !path.is_empty() {
        out.push(',');
        push_u64(out, "steps", path.len() as u64);
        out.push(',');
        out.push_str("\"path\":[");
        for (i, k) in path.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            push_json_string(out, k);
        }
        out.push(']');
        out.push(',');
        push_bool(out, "reachable", true);
    } else if !nav.can_act {
        out.push(',');
        push_bool(out, "reachable", false);
    }
    out.push('}');
}

fn metric(a: (i32, i32), b: (i32, i32)) -> i32 {
    (a.0 - b.0).abs() + (a.1 - b.1).abs() * 2
}

/// Accurate search range (mirrors fullgame_autoplay / classic engine).
fn can_search_from(position: (i32, i32), event: (i32, i32), mode: u16) -> bool {
    let limit = (mode as usize * 6).saturating_sub(4).min(13);
    if limit == 0 {
        return false;
    }
    for direction in 0..4 {
        let (x_offset, y_offset) = match direction {
            0 => (-16, 8),
            1 => (-16, -8),
            2 => (16, -8),
            _ => (16, 8),
        };
        let (mut x, mut y) = position;
        let mut range = [(0i32, 0i32); 13];
        range[0] = position;
        for index in 0..4 {
            range[index * 3 + 1] = (x + x_offset, y + y_offset);
            range[index * 3 + 2] = (x, y + y_offset * 2);
            range[index * 3 + 3] = (x + x_offset * 2, y);
            x += x_offset;
            y += y_offset;
        }
        if range[..limit].contains(&event) {
            return true;
        }
    }
    false
}

fn script_destination_scene(engine: &Engine, script: u16) -> Option<u16> {
    if script == 0 {
        return None;
    }
    let start = script as usize;
    for index in start..start.saturating_add(24) {
        let entry = engine.globals.game.script_entries.get(index)?;
        // 0x0059 = teleport / change scene (classic).
        if entry.operation == 0x0059 && entry.operand[0] != 0 {
            return Some(entry.operand[0]);
        }
        if entry.operation == 0x0000 {
            break;
        }
    }
    None
}

/// BFS short path; returns key names (up/right/down/left), truncated.
fn path_to_event(
    engine: &Engine,
    start: (i32, i32),
    event: crate::global::EventObject,
    bfs_limit: usize,
    max_steps: usize,
) -> Vec<&'static str> {
    let goal = (event.x as i32, event.y as i32);
    let trigger_distance = if event.trigger_mode >= 4 {
        ((event.trigger_mode - 4) as i32 * 32 + 16).max(16)
    } else {
        0
    };
    let reached = |position: (i32, i32)| {
        if event.trigger_mode > 0 && event.trigger_mode < 4 {
            can_search_from(position, goal, event.trigger_mode)
        } else if event.trigger_mode >= 4 {
            metric(position, goal) < trigger_distance
        } else {
            metric(position, goal) < 24
        }
    };
    if reached(start) {
        return Vec::new();
    }

    let mut queue = VecDeque::from([start]);
    let mut previous: HashMap<(i32, i32), ((i32, i32), usize)> = HashMap::new();
    let mut seen = HashSet::from([start]);
    let mut found = None;

    while let Some(position) = queue.pop_front() {
        if reached(position) {
            found = Some(position);
            break;
        }
        if seen.len() > bfs_limit {
            break;
        }
        for (di, &((dx, dy), _)) in WALK_DELTA.iter().enumerate() {
            let next = (position.0 + dx, position.1 + dy);
            if !(0..8192).contains(&next.0) || !(0..4096).contains(&next.1) {
                continue;
            }
            if seen.contains(&next) {
                continue;
            }
            if engine.check_obstacle_with_range(next, true, 0, true) {
                continue;
            }
            seen.insert(next);
            previous.insert(next, (position, di));
            queue.push_back(next);
        }
    }

    let Some(mut at) = found else {
        // Fallback: try ignoring event-object collision once.
        return path_to_event_loose(engine, start, event, bfs_limit / 2, max_steps);
    };
    let mut rev: Vec<usize> = Vec::new();
    while at != start {
        let Some(&(before, di)) = previous.get(&at) else {
            break;
        };
        rev.push(di);
        at = before;
    }
    rev.reverse();
    rev.truncate(max_steps);
    rev.into_iter()
        .map(|di| WALK_DELTA[di].1)
        .collect()
}

fn path_to_event_loose(
    engine: &Engine,
    start: (i32, i32),
    event: crate::global::EventObject,
    bfs_limit: usize,
    max_steps: usize,
) -> Vec<&'static str> {
    let goal = (event.x as i32, event.y as i32);
    let trigger_distance = if event.trigger_mode >= 4 {
        ((event.trigger_mode - 4) as i32 * 32 + 16).max(16)
    } else {
        0
    };
    let reached = |position: (i32, i32)| {
        if event.trigger_mode > 0 && event.trigger_mode < 4 {
            can_search_from(position, goal, event.trigger_mode)
        } else if event.trigger_mode >= 4 {
            metric(position, goal) < trigger_distance
        } else {
            metric(position, goal) < 24
        }
    };
    let mut queue = VecDeque::from([start]);
    let mut previous: HashMap<(i32, i32), ((i32, i32), usize)> = HashMap::new();
    let mut seen = HashSet::from([start]);
    let mut found = None;
    while let Some(position) = queue.pop_front() {
        if reached(position) {
            found = Some(position);
            break;
        }
        if seen.len() > bfs_limit {
            break;
        }
        for (di, &((dx, dy), _)) in WALK_DELTA.iter().enumerate() {
            let next = (position.0 + dx, position.1 + dy);
            if !(0..8192).contains(&next.0) || !(0..4096).contains(&next.1) {
                continue;
            }
            if seen.contains(&next) {
                continue;
            }
            // Ignore event objects as blockers.
            if engine.check_obstacle_with_range(next, false, 0, true) {
                continue;
            }
            seen.insert(next);
            previous.insert(next, (position, di));
            queue.push_back(next);
        }
    }
    let Some(mut at) = found else {
        return Vec::new();
    };
    let mut rev: Vec<usize> = Vec::new();
    while at != start {
        let Some(&(before, di)) = previous.get(&at) else {
            break;
        };
        rev.push(di);
        at = before;
    }
    rev.reverse();
    rev.truncate(max_steps);
    rev.into_iter()
        .map(|di| WALK_DELTA[di].1)
        .collect()
}

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

fn build_hint(
    engine: &Engine,
    phase: &str,
    in_dialog: bool,
    in_battle: bool,
    in_menu: bool,
    nav: &NavInfo,
    path: &[&'static str],
    walk: &[bool; 4],
) -> String {
    if engine.quit_requested {
        return "quit requested — stop".into();
    }
    if in_dialog || phase == "dialog" {
        let preview = {
            let s = &engine.ui.agent_dialog_speaker;
            let lines = &engine.ui.agent_dialog_lines;
            if !lines.is_empty() {
                let body = lines.join(" ");
                let short: String = body.chars().take(24).collect();
                if s.is_empty() {
                    short
                } else {
                    format!("{s}：{short}")
                }
            } else {
                String::new()
            }
        };
        if preview.is_empty() {
            return "dialog — press confirm".into();
        }
        return format!("dialog — confirm ({preview})");
    }
    if in_menu || phase == "menu" {
        if let Some(menu) = engine.ui.agent_menu.as_ref() {
            let cur = menu
                .items
                .get(menu.index)
                .map(|it| it.label.as_str())
                .unwrap_or("?");
            return format!(
                "menu[{}] index={} 「{}」 — arrows+confirm, menu=cancel",
                menu.kind, menu.index, cur
            );
        }
        if let Some(battle) = engine.battle.as_ref() {
            if battle.ui.state == BattleUiState::SelectMove {
                return format!(
                    "battle menu {} — arrows+confirm",
                    battle_menu_state_name(battle.ui.menu_state)
                );
            }
        }
        return "menu — arrows+confirm, menu=cancel".into();
    }
    if in_battle || phase == "battle" {
        if let Some(battle) = engine.battle.as_ref() {
            match battle.ui.state {
                BattleUiState::SelectTargetEnemy | BattleUiState::SelectTargetEnemyAll => {
                    let idx = battle.ui.selected_index;
                    let name = battle
                        .enemy
                        .get(idx as usize)
                        .filter(|e| e.object_id != 0)
                        .map(|e| word_utf8(engine, e.object_id as usize))
                        .unwrap_or_else(|| "?".into());
                    return format!(
                        "select enemy target index={idx} ({name}) — left/right, confirm"
                    );
                }
                BattleUiState::SelectTargetPlayer | BattleUiState::SelectTargetPlayerAll => {
                    return format!(
                        "select ally target index={} — left/right, confirm",
                        battle.ui.selected_index
                    );
                }
                BattleUiState::SelectMove => {
                    return format!(
                        "battle turn player={} menu={} — confirm/force/auto/defend",
                        battle.ui.cur_player_index,
                        battle_menu_state_name(battle.ui.menu_state)
                    );
                }
                BattleUiState::Wait => {
                    return "battle wait — hold or step".into();
                }
            }
        }
        return "battle — confirm/force/auto".into();
    }
    if phase == "scene_transition" || engine.globals.entering_scene {
        return "scene transition — wait/step".into();
    }
    if phase == "boot" || !engine.globals.in_main_game {
        return "boot/title — confirm; if step_mode, step frames".into();
    }
    // Overworld.
    if nav.event_id != 0 {
        if nav.can_act {
            return format!(
                "at event #{} ({}) — confirm/space to interact",
                nav.event_id, nav.role
            );
        }
        if !path.is_empty() {
            let preview: Vec<&str> = path.iter().copied().take(6).collect();
            return format!(
                "go to #{} ({}) path={}… — press {}",
                nav.event_id,
                nav.role,
                preview.join(">"),
                path[0]
            );
        }
        if !nav.keys.is_empty() {
            return format!(
                "approach #{} ({}) keys={} — press {}",
                nav.event_id,
                nav.role,
                nav.keys.join("/"),
                nav.keys[0]
            );
        }
        // Walkable dirs as fallback.
        let open: Vec<&str> = WALK_DELTA
            .iter()
            .enumerate()
            .filter(|(i, _)| walk[*i])
            .map(|(_, n)| n.1)
            .collect();
        if open.is_empty() {
            return format!(
                "blocked near #{} ({}) — try menu or other event",
                nav.event_id, nav.role
            );
        }
        return format!(
            "approach #{} ({}) — try {}",
            nav.event_id,
            nav.role,
            open.join("/")
        );
    }
    let open: Vec<&str> = WALK_DELTA
        .iter()
        .enumerate()
        .filter(|(i, _)| walk[*i])
        .map(|(_, n)| n.1)
        .collect();
    if open.is_empty() {
        "overworld stuck — confirm nearby or menu".into()
    } else {
        format!("explore — walk {}", open.join("/"))
    }
}

fn append_keys_hint(
    out: &mut String,
    phase: &str,
    in_dialog: bool,
    in_battle: bool,
    in_menu: bool,
    nav: &NavInfo,
    path: &[&'static str],
) {
    out.push('[');
    let mut hints: Vec<&str> = Vec::new();
    if in_dialog || phase == "dialog" {
        hints.push("confirm");
    } else if in_menu || phase == "menu" {
        hints.extend_from_slice(&["up", "down", "left", "right", "confirm", "menu"]);
    } else if in_battle || phase == "battle" {
        hints.extend_from_slice(&["up", "down", "left", "right", "confirm", "menu", "force", "auto", "defend"]);
    } else if phase == "boot" {
        hints.push("confirm");
    } else if nav.can_act {
        hints.extend_from_slice(&["confirm", "space"]);
    } else if let Some(&k) = path.first() {
        hints.push(k);
    } else if !nav.keys.is_empty() {
        for k in &nav.keys {
            hints.push(k);
        }
        hints.push("confirm");
    } else {
        hints.extend_from_slice(&["up", "down", "left", "right", "confirm", "space", "menu"]);
    }
    for (i, h) in hints.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(h);
        out.push('"');
    }
    out.push(']');
}

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

//! Rich JSON snapshot for `GET /v1/state` (AI / HTTP driver).
//!
//! Kept out of `game_loop.rs` so the observe surface can grow without
//! cluttering the engine core.

use crate::battle::{BattleMenuState, BattlePhase, BattleUiState, FighterState};
use crate::game_loop::Engine;
use crate::global::{
    ITEMFLAG_APPLY_TO_ALL, ITEMFLAG_CONSUMING, ITEMFLAG_EQUIPABLE, ITEMFLAG_SELLABLE,
    ITEMFLAG_THROWABLE, ITEMFLAG_USABLE, MAX_ENEMIES_IN_TEAM, MAX_INVENTORY,
    MAX_PLAYABLE_PLAYER_ROLES, MAX_PLAYER_EQUIPMENTS, MAX_PLAYER_MAGICS, MAX_PLAYER_ROLES,
    MAX_PLAYERS_IN_PARTY,
};
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

pub(crate) fn build_state_json(engine: &Engine) -> String {
    let frame_id = ui_driver::latest_frame_id();
    let step_mode = ui_driver::step_mode_enabled();
    let step_configured = ui_driver::step_mode_configured();
    let g = &engine.globals;
    let player = (
        g.viewport.0 + g.partyoffset.0,
        g.viewport.1 + g.partyoffset.1,
    );
    let in_dialog = engine.ui.in_dialog || engine.ui.current_dialog_line > 0;
    let in_battle = g.in_battle || engine.battle.is_some();

    let phase = if !g.in_main_game {
        "boot"
    } else if in_battle {
        "battle"
    } else if in_dialog {
        "dialog"
    } else if g.entering_scene {
        "scene_transition"
    } else {
        "overworld"
    };

    let mut out = String::with_capacity(8 << 10);
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
    push_pair(&mut out, "party_offset", g.partyoffset.0, g.partyoffset.1);
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
    push_bool(&mut out, "in_dialog", in_dialog);
    out.push(',');
    push_i64(&mut out, "dialog_line", engine.ui.current_dialog_line as i64);
    out.push(',');
    push_u64(&mut out, "dialog_position", engine.ui.dialog_position as u64);
    out.push(',');
    push_bool(&mut out, "quit_requested", engine.quit_requested);
    out.push(',');
    push_u64(&mut out, "cash", g.cash as u64);
    out.push(',');
    push_u64(&mut out, "collect_value", g.collect_value as u64);
    out.push(',');
    push_u64(&mut out, "current_save_slot", g.current_save_slot as u64);
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

    // Four-way walkability (same collision as party movement).
    out.push(',');
    out.push_str("\"walk\":{");
    for (i, &((dx, dy), name)) in WALK_DELTA.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let blocked =
            engine.check_obstacle_with_range((player.0 + dx, player.1 + dy), true, 0, true);
        push_bool(&mut out, name, !blocked);
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

    // Nearby / active event objects in this scene.
    out.push(',');
    out.push_str("\"events\":");
    append_events(&mut out, engine, player);

    // Battle block (null when not in battle).
    out.push(',');
    out.push_str("\"battle\":");
    if let Some(battle) = engine.battle.as_ref() {
        append_battle(&mut out, engine, battle);
    } else {
        out.push_str("null");
    }

    // Suggested keys for the current phase (hints only).
    out.push(',');
    out.push_str("\"keys_hint\":");
    append_keys_hint(&mut out, phase, in_dialog, in_battle);

    // Compact action vocabulary reminder.
    out.push(',');
    out.push_str(
        "\"actions\":[\"up\",\"down\",\"left\",\"right\",\"confirm\",\"space\",\"menu\",\
         \"force\",\"auto\",\"defend\",\"use_item\",\"throw_item\",\"flee\",\"status\",\
         \"repeat\",\"page_up\",\"page_down\"]",
    );

    out.push('}');
    out.push('\n');
    out
}

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
        push_pair(out, "sprite_pos", g.party[i].x as i32, g.party[i].y as i32);
        out.push(',');
        // Equipment object ids + names.
        out.push_str("\"equipment\":[");
        for e in 0..MAX_PLAYER_EQUIPMENTS {
            if e > 0 {
                out.push(',');
            }
            let item = roles.equipment[e][role];
            out.push('{');
            push_u64(out, "slot", e as u64);
            out.push(',');
            push_u64(out, "item", item as u64);
            out.push(',');
            push_str(out, "name", &word_utf8(engine, item as usize));
            out.push('}');
        }
        out.push_str("],");
        // Learned magics (object ids).
        out.push_str("\"magics\":[");
        let mut first_m = true;
        for m in 0..MAX_PLAYER_MAGICS {
            let mid = roles.magic[m][role];
            if mid == 0 {
                continue;
            }
            if !first_m {
                out.push(',');
            }
            first_m = false;
            out.push('{');
            push_u64(out, "id", mid as u64);
            out.push(',');
            push_str(out, "name", &word_utf8(engine, mid as usize));
            out.push('}');
        }
        out.push_str("],");
        // Status timers (poison etc. — raw array).
        out.push_str("\"status\":[");
        for s in 0..crate::global::STATUS_ALL {
            if s > 0 {
                out.push(',');
            }
            out.push_str(&g.player_status[role][s].to_string());
        }
        out.push(']');
        out.push('}');
    }
    out.push(']');
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
        out.push(',');
        push_u64(out, "amount_in_use", inv.amount_in_use as u64);
        out.push(',');
        push_u64(out, "flags", flags as u64);
        out.push(',');
        push_bool(out, "usable", flags & ITEMFLAG_USABLE != 0);
        out.push(',');
        push_bool(out, "equipable", flags & ITEMFLAG_EQUIPABLE != 0);
        out.push(',');
        push_bool(out, "throwable", flags & ITEMFLAG_THROWABLE != 0);
        out.push(',');
        push_bool(out, "consuming", flags & ITEMFLAG_CONSUMING != 0);
        out.push(',');
        push_bool(out, "apply_to_all", flags & ITEMFLAG_APPLY_TO_ALL != 0);
        out.push(',');
        push_bool(out, "sellable", flags & ITEMFLAG_SELLABLE != 0);
        out.push('}');
    }
    out.push(']');
}

fn append_events(out: &mut String, engine: &Engine, player: (i32, i32)) {
    let g = &engine.globals;
    let scene_i = g.num_scene as usize;
    if scene_i == 0 || scene_i > g.game.scenes.len() {
        out.push_str("[]");
        return;
    }
    let start = g.game.scenes[scene_i - 1].event_object_index as usize;
    let end = g
        .game
        .scenes
        .get(scene_i)
        .map(|s| s.event_object_index as usize)
        .unwrap_or(g.game.event_objects.len());

    let mut rows: Vec<(i32, u16, usize)> = Vec::new();
    for index in start..end.min(g.game.event_objects.len()) {
        let ev = g.game.event_objects[index];
        if ev.state <= 0 || ev.vanish_time != 0 {
            continue;
        }
        // Skip pure scenery with no scripts unless close.
        let event_id = (index + 1) as u16;
        let pos = (ev.x as i32, ev.y as i32);
        let dist = (player.0 - pos.0).abs() + (player.1 - pos.1).abs() * 2;
        if dist > 400 && ev.trigger_script == 0 && ev.auto_script == 0 {
            continue;
        }
        rows.push((dist, event_id, index));
    }
    rows.sort_by_key(|&(d, id, _)| (d, id));
    rows.truncate(MAX_EVENTS);

    out.push('[');
    for (i, &(dist, event_id, index)) in rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let ev = g.game.event_objects[index];
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
        push_i64(out, "dist", dist as i64);
        out.push(',');
        push_i64(out, "state", ev.state as i64);
        out.push(',');
        push_u64(out, "trigger_mode", ev.trigger_mode as u64);
        out.push(',');
        push_u64(out, "trigger_script", ev.trigger_script as u64);
        out.push(',');
        push_u64(out, "auto_script", ev.auto_script as u64);
        out.push(',');
        push_u64(out, "sprite_num", ev.sprite_num as u64);
        out.push(',');
        push_u64(out, "direction", ev.direction as u64);
        out.push(',');
        push_i64(out, "layer", ev.layer as i64);
        out.push(',');
        // Interact range hint for search triggers (mode 1..3).
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
        push_bool(out, "can_search_now", search_ok);
        out.push(',');
        push_bool(out, "in_touch_range", in_touch);
        out.push('}');
    }
    out.push(']');
}

fn can_search_from(player: (i32, i32), event: (i32, i32), mode: u16) -> bool {
    // Mirrors fullgame_autoplay::can_search_event_from (simplified).
    let mode = mode.max(1);
    let dx = player.0 - event.0;
    let dy = player.1 - event.1;
    let range = mode as i32 * 16 + 8;
    dx.abs() + dy.abs() * 2 <= range
}

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
        out.push('{');
        push_u64(out, "index", i as u64);
        out.push(',');
        push_u64(out, "object_id", e.object_id as u64);
        out.push(',');
        push_str(out, "name", &word_utf8(engine, e.object_id as usize));
        out.push(',');
        push_u64(out, "hp", e.e.health as u64);
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
        out.push('{');
        push_u64(out, "slot", i as u64);
        out.push(',');
        push_str(out, "state", fighter_state_name(p.state));
        out.push(',');
        push_f64(out, "time_meter", f64::from(p.time_meter));
        out.push(',');
        push_bool(out, "defending", p.defending);
        out.push(',');
        push_u64(out, "prev_hp", p.prev_hp as u64);
        out.push(',');
        push_u64(out, "prev_mp", p.prev_mp as u64);
        out.push('}');
    }
    out.push(']');
    out.push('}');
}

fn append_keys_hint(out: &mut String, phase: &str, in_dialog: bool, in_battle: bool) {
    out.push('[');
    let hints: &[&str] = if in_dialog || phase == "dialog" {
        &["confirm"]
    } else if in_battle || phase == "battle" {
        &["force", "auto", "defend", "confirm", "menu"]
    } else if phase == "boot" {
        &["confirm"]
    } else {
        &["up", "down", "left", "right", "confirm", "space", "menu"]
    };
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
    bytes_to_display(&engine.texts.word(n))
}

fn bytes_to_display(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    // Strip dialog control-ish ASCII for names; keep printable.
    let (cow, _, _) = encoding_rs::BIG5.decode(bytes);
    cow.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect()
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

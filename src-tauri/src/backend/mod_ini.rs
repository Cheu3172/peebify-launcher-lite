// ------------ Mod Loader Settings ------------
// The few mod loader (d3dx.ini) options players can change from the UI, like the reload shortcut and the start
// delay. Values are checked here, saved in the config and written into the loader's ini file.

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::state::BackendState;
use super::{arg_str, err_response, game_profiles, ok_with, resolve_profile, xxmi};

pub enum Kind {
    Bool,
    Millis { min: u32, max: u32 },
    Key,
}

pub struct IniSetting {
    pub id: &'static str,
    pub section: &'static str,
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub kind: Kind,
    pub shipped: &'static str,
    pub mirrors: &'static [(&'static str, &'static str)],
}

pub const SETTINGS: &[IniSetting] = &[
    IniSetting {
        id: "reloadMods",
        section: "Hunting",
        key: "reload_fixes",
        label: "Reload mods in game",
        description:
            "The mod loader's own shortcut. It re-reads every enabled mod without restarting the game.",
        kind: Kind::Key,
        shipped: "no_modifiers VK_F10",
        mirrors: &[("Hunting", "reload_config")],
    },
    IniSetting {
        id: "showWarnings",
        section: "Logging",
        key: "show_warnings",
        label: "Show mod warnings in game",
        description:
            "Red text the mod loader prints on screen when a mod's ini has a problem. Off by default, because nearly all of it comes from the mods themselves.",
        kind: Kind::Bool,
        shipped: "0",
        mirrors: &[],
    },
    IniSetting {
        id: "injectionDelay",
        section: "System",
        key: "dll_initialization_delay",
        label: "Mod library start delay",
        description:
            "How long the mod library waits inside the game before it starts, in milliseconds. Raise it if the game crashes on launch with mods on.",
        kind: Kind::Millis { min: 0, max: 10000 },
        shipped: "0",
        mirrors: &[],
    },
];

fn setting(id: &str) -> Option<&'static IniSetting> {
    SETTINGS.iter().find(|s| s.id == id)
}

pub fn validate(spec: &IniSetting, raw: &str) -> Result<String, &'static str> {
    match spec.kind {
        Kind::Bool => match raw.trim() {
            "true" | "1" => Ok("1".to_string()),
            "false" | "0" => Ok("0".to_string()),
            _ => Err("That setting is on or off."),
        },
        Kind::Millis { min, max } => raw
            .trim()
            .parse::<u32>()
            .map(|n| n.clamp(min, max).to_string())
            .map_err(|_| "That setting is a whole number of milliseconds."),
        Kind::Key => accelerator_to_ini(raw).ok_or(
            "The mod loader only takes Ctrl, Alt and Shift with a function key or a single letter.",
        ),
    }
}

pub fn ini_text_is_safe(text: &str) -> bool {
    !text.contains(['\r', '\n', '[', ']'])
}

pub fn sanitize_stored(
    compound: &str,
    value: &Value,
) -> Option<(&'static str, &'static str, String)> {
    let (section, key) = compound.split_once('/')?;
    let (section, key) = (section.trim(), key.trim());
    let (spec, target) = SETTINGS.iter().find_map(|spec| {
        std::iter::once((spec.section, spec.key))
            .chain(spec.mirrors.iter().copied())
            .find(|(s, k)| s.eq_ignore_ascii_case(section) && k.eq_ignore_ascii_case(key))
            .map(|target| (spec, target))
    })?;
    let raw = match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => u8::from(*b).to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    if !ini_text_is_safe(&raw) {
        return None;
    }
    let normalized = match spec.kind {
        Kind::Key => accelerator_to_ini(&ini_to_accelerator(&raw))?,
        _ => validate(spec, &raw).ok()?,
    };
    Some((target.0, target.1, normalized))
}

const KEY_MODIFIERS: [(&str, &str); 3] = [("ctrl", "Ctrl"), ("alt", "Alt"), ("shift", "Shift")];

pub fn accelerator_to_ini(text: &str) -> Option<String> {
    let parts: Vec<&str> = text
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let (key, mods) = parts.split_last()?;
    let mut out: Vec<String> = Vec::new();
    for m in mods {
        let lower = m.to_lowercase();
        if !KEY_MODIFIERS.iter().any(|(ini, _)| *ini == lower) {
            return None;
        }
        out.push(lower);
    }
    if out.is_empty() {
        out.push("no_modifiers".to_string());
    }
    let named = key.to_uppercase();
    let key = if named.starts_with('F')
        && named[1..]
            .parse::<u8>()
            .is_ok_and(|n| (1..=24).contains(&n))
    {
        format!("VK_{named}")
    } else if named.len() == 1 && named.chars().all(|c| c.is_ascii_alphanumeric()) {
        named
    } else {
        return None;
    };
    out.push(key);
    Some(out.join(" "))
}

pub fn ini_to_accelerator(text: &str) -> String {
    let mut mods: Vec<&str> = Vec::new();
    let mut key = String::new();
    for token in text.split_whitespace() {
        let lower = token.to_lowercase();
        match KEY_MODIFIERS.iter().find(|(ini, _)| *ini == lower) {
            Some((_, label)) => mods.push(label),
            None if lower == "no_modifiers" => {}
            None => {
                key = token
                    .trim_start_matches("VK_")
                    .trim_start_matches("vk_")
                    .to_string()
            }
        }
    }
    mods.push(&key);
    mods.join("+")
}

fn read_ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed[1..trimmed.len() - 1]
                .trim()
                .eq_ignore_ascii_case(section);
            continue;
        }
        if !in_section || trimmed.starts_with(';') || trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            if k.trim().eq_ignore_ascii_case(key) {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

fn variant_for(app: &AppHandle, game_id: &str) -> Option<(String, std::path::PathBuf)> {
    let state = app.state::<BackendState>();
    let profile = game_profiles::profile(game_id);
    let variant = game_profiles::mod_variant(profile)?.to_string();
    let root = xxmi::root(&state.config, &state.user_data);
    Some((variant.clone(), root.join(&variant)))
}

pub(super) async fn mod_ini_settings(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let profile = resolve_profile(app, arg_str(args, 0));
    let game_id = game_profiles::profile_id(profile).to_string();
    let state = app.state::<BackendState>();

    let Some((variant, dir)) = variant_for(app, &game_id) else {
        return Ok(ok_with(json!({
            "gameId": game_id,
            "supported": false,
            "toolchainInstalled": false,
            "settings": Vec::<Value>::new(),
        })));
    };

    let installed = xxmi::variant_installed(&xxmi::root(&state.config, &state.user_data), &variant);
    let text = std::fs::read_to_string(dir.join("d3dx.ini")).unwrap_or_default();
    let defaults = game_profiles::mod_ini_defaults(profile);

    let rows: Vec<Value> = SETTINGS
        .iter()
        .map(|s| {
            let shipped = defaults
                .iter()
                .find(|(section, key, _)| {
                    section.eq_ignore_ascii_case(s.section) && key.eq_ignore_ascii_case(s.key)
                })
                .map(|(_, _, value)| value.as_str())
                .unwrap_or(s.shipped);
            let compound = format!("{}/{}", s.section, s.key);
            let stored = state
                .config
                .get(&format!("games.{game_id}.modIni.{compound}"));
            let stored = sanitize_stored(&compound, &stored).map(|(_, _, v)| v);
            let value = stored
                .or_else(|| read_ini_value(&text, s.section, s.key))
                .unwrap_or_else(|| shipped.to_string());
            json!({
                "id": s.id,
                "label": s.label,
                "description": s.description,
                "kind": match s.kind {
                    Kind::Bool => "bool",
                    Kind::Millis { .. } => "millis",
                    Kind::Key => "key",
                },
                "accelerator": matches!(s.kind, Kind::Key).then(|| ini_to_accelerator(&value)),
                "defaultAccelerator": matches!(s.kind, Kind::Key)
                    .then(|| ini_to_accelerator(shipped)),
                "min": match s.kind { Kind::Millis { min, .. } => Some(min), _ => None },
                "max": match s.kind { Kind::Millis { max, .. } => Some(max), _ => None },
                "value": value,
            })
        })
        .collect();

    Ok(ok_with(json!({
        "gameId": game_id,
        "supported": true,
        "toolchainInstalled": installed,
        "settings": rows,
    })))
}

pub(super) async fn set_mod_ini_setting(app: &AppHandle, args: &[Value]) -> Result<Value, String> {
    let profile = resolve_profile(app, arg_str(args, 0));
    let game_id = game_profiles::profile_id(profile).to_string();

    let Some(id) = arg_str(args, 1) else {
        return Ok(err_response("No setting was given."));
    };
    let Some(spec) = setting(id) else {
        return Ok(err_response("That is not a mod loader setting."));
    };
    let Some(raw) = arg_str(args, 2) else {
        return Ok(err_response("No value was given."));
    };

    let value = match validate(spec, raw) {
        Ok(value) => value,
        Err(message) => return Ok(err_response(message)),
    };

    let config = &app.state::<BackendState>().config;
    for (section, key) in
        std::iter::once((spec.section, spec.key)).chain(spec.mirrors.iter().copied())
    {
        config.set(
            &format!("games.{game_id}.modIni.{section}/{key}"),
            Value::String(value.clone()),
        );
    }

    let applied = match variant_for(app, &game_id) {
        Some((variant, dir)) => {
            let state = app.state::<BackendState>();
            let root = xxmi::root(&state.config, &state.user_data);
            if xxmi::variant_installed(&root, &variant) {
                let overrides = xxmi::stored_overrides(&state.config, &game_id);
                xxmi::apply_ini_overrides(&dir, &overrides).unwrap_or(false)
            } else {
                false
            }
        }
        None => false,
    };

    let _ = app.emit(
        "mod-ini-changed",
        json!({ "gameId": game_id, "settingId": spec.id, "value": value, "applied": applied }),
    );

    Ok(ok_with(json!({
        "settingId": spec.id,
        "value": value,
        "applied": applied,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_values_outside_the_declared_settings_are_dropped() {
        assert_eq!(
            sanitize_stored("System/proxy_d3d11", &json!(r"\\host\share\x.dll")),
            None
        );
        assert_eq!(sanitize_stored("Loader/target", &json!("Evil.exe")), None);
        assert_eq!(sanitize_stored("no compound", &json!("1")), None);
    }

    #[test]
    fn declared_settings_are_normalized_by_kind() {
        assert_eq!(
            sanitize_stored("Hunting/reload_fixes", &json!("no_modifiers VK_F10")),
            Some(("Hunting", "reload_fixes", "no_modifiers VK_F10".to_string()))
        );
        assert_eq!(
            sanitize_stored("hunting/RELOAD_CONFIG", &json!("ctrl alt VK_F5")),
            Some(("Hunting", "reload_config", "ctrl alt VK_F5".to_string()))
        );
        assert_eq!(
            sanitize_stored("Logging/show_warnings", &json!(true)),
            Some(("Logging", "show_warnings", "1".to_string()))
        );
        assert_eq!(
            sanitize_stored("System/dll_initialization_delay", &json!("99999")),
            Some(("System", "dll_initialization_delay", "10000".to_string()))
        );
        assert_eq!(sanitize_stored("Logging/show_warnings", &json!("yes")), None);
    }

    #[test]
    fn stored_values_cannot_smuggle_lines_or_sections() {
        assert_eq!(
            sanitize_stored(
                "Hunting/reload_fixes",
                &json!("no_modifiers VK_F10\r\n[System]\nproxy_d3d11 = x.dll")
            ),
            None
        );
        assert_eq!(
            sanitize_stored("Hunting/reload_fixes", &json!(r"no_modifiers \\host\x.dll")),
            None
        );
        assert_eq!(
            sanitize_stored("System/dll_initialization_delay", &json!("5\n[x]")),
            None
        );
    }
}

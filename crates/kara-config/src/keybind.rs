/// Keybind parsing: modifier+key combos, action names, {1-9} range expansion.

use crate::types::{BindAction, Keybind, ModMask};

/// Parse a key combo string like "mod+Shift+Return" into (ModMask, keysym).
pub fn parse_combo(combo: &str) -> Result<(ModMask, u32), String> {
    let mut mods = ModMask::NONE;
    let mut sym = None;

    for part in combo.split('+') {
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "mod" | "super" | "win" => mods.logo = true,
            "shift" => mods.shift = true,
            "ctrl" | "control" => mods.ctrl = true,
            "alt" => mods.alt = true,
            _ => {
                // Key name — resolve via xkbcommon
                let keysym = keysym_from_name(part)?;
                sym = Some(keysym);
            }
        }
    }

    match sym {
        Some(s) => Ok((mods, s)),
        None => Err(format!("no key in combo '{combo}'")),
    }
}

/// Resolve a key name to an xkbcommon keysym value.
///
/// Uses xkb_keysym_from_name with case-insensitive fallback.
/// Names follow X11 conventions: "Return", "bracketleft", "XF86AudioRaiseVolume", "a", "1", etc.
fn keysym_from_name(name: &str) -> Result<u32, String> {
    let sym = xkbcommon::xkb::keysym_from_name(name, xkbcommon::xkb::KEYSYM_NO_FLAGS);
    if sym.raw() != 0 {
        return Ok(sym.raw());
    }

    // Case-insensitive retry
    let sym = xkbcommon::xkb::keysym_from_name(name, xkbcommon::xkb::KEYSYM_CASE_INSENSITIVE);
    if sym.raw() != 0 {
        return Ok(sym.raw());
    }

    Err(format!("unknown key '{name}'"))
}

/// Parse an action name + optional arg into a BindAction.
pub fn parse_action(verb: &str, arg: Option<&str>) -> Result<BindAction, String> {
    match verb {
        "spawn" => {
            let name = arg.ok_or("spawn requires a command name")?;
            Ok(BindAction::Spawn(name.to_string()))
        }
        "scratchpad" => Ok(BindAction::Scratchpad(arg.map(|s| s.to_string()))),
        "focus_next" => Ok(BindAction::FocusNext),
        "focus_prev" => Ok(BindAction::FocusPrev),
        "focus_monitor_prev" => Ok(BindAction::FocusMonitorPrev),
        "focus_monitor_next" => Ok(BindAction::FocusMonitorNext),
        "send_monitor_prev" => Ok(BindAction::SendMonitorPrev),
        "send_monitor_next" => Ok(BindAction::SendMonitorNext),
        "decrease_mfact" => Ok(BindAction::DecreaseMfact),
        "increase_mfact" => Ok(BindAction::IncreaseMfact),
        "zoom_master" => Ok(BindAction::ZoomMaster),
        "monocle" | "toggle_monocle" => Ok(BindAction::Monocle),
        "fullscreen" | "toggle_fullscreen" => Ok(BindAction::Fullscreen),
        "toggle_sync" | "sync" => Ok(BindAction::ToggleSync),
        "kill_client" | "kill" => Ok(BindAction::KillClient),
        "reload" => Ok(BindAction::Reload),
        "quit" => Ok(BindAction::Quit),
        "view_ws" => {
            let n: usize = arg
                .ok_or("view_ws requires a workspace number")?
                .parse()
                .map_err(|_| "invalid workspace number")?;
            if !(1..=9).contains(&n) {
                return Err(format!("workspace number {n} out of range 1-9"));
            }
            Ok(BindAction::ViewWs(n - 1))
        }
        "send_ws" => {
            let n: usize = arg
                .ok_or("send_ws requires a workspace number")?
                .parse()
                .map_err(|_| "invalid workspace number")?;
            if !(1..=9).contains(&n) {
                return Err(format!("workspace number {n} out of range 1-9"));
            }
            Ok(BindAction::SendWs(n - 1))
        }
        _ => Err(format!("unknown action '{verb}'")),
    }
}

/// Expand `{1-9}` ranges in a bind line.
///
/// Given tokens like `["mod+{1-9}", "view_ws", "{1-9}"]`, produces 9 sets of
/// tokens with {1-9} replaced by 1, 2, ..., 9.
///
/// Returns the expanded keybinds or the single original if no range found.
pub fn expand_range_binds(tokens: &[&str]) -> Result<Vec<Keybind>, String> {
    let has_range = tokens.iter().any(|t| t.contains("{1-9}"));

    if !has_range {
        // No range — parse as single keybind
        return parse_single_bind(tokens).map(|kb| vec![kb]);
    }

    let mut result = Vec::with_capacity(9);
    for n in 1..=9u32 {
        let n_str = n.to_string();
        let expanded: Vec<String> = tokens
            .iter()
            .map(|t| t.replace("{1-9}", &n_str))
            .collect();
        let refs: Vec<&str> = expanded.iter().map(|s| s.as_str()).collect();
        result.push(parse_single_bind(&refs)?);
    }
    Ok(result)
}

/// Parse a single bind line (already tokenized, no ranges).
/// Format: "combo" action [arg]
fn parse_single_bind(tokens: &[&str]) -> Result<Keybind, String> {
    if tokens.len() < 2 {
        return Err("bind requires at least a key combo and action".into());
    }

    let (mods, keysym) = parse_combo(tokens[0])?;
    let verb = tokens[1];
    let arg = tokens.get(2).copied();
    let action = parse_action(verb, arg)?;

    Ok(Keybind { mods, keysym, action })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_combo_simple() {
        let (mods, sym) = parse_combo("mod+Return").unwrap();
        assert!(mods.logo);
        assert!(!mods.shift);
        assert_eq!(sym, 0xff0d); // XK_Return
    }

    #[test]
    fn test_parse_combo_shift() {
        let (mods, sym) = parse_combo("mod+Shift+q").unwrap();
        assert!(mods.logo);
        assert!(mods.shift);
        assert_eq!(sym, 0x71); // XK_q
    }

    #[test]
    fn test_parse_action_spawn() {
        let action = parse_action("spawn", Some("terminal")).unwrap();
        assert_eq!(action, BindAction::Spawn("terminal".into()));
    }

    #[test]
    fn test_parse_action_view_ws() {
        let action = parse_action("view_ws", Some("3")).unwrap();
        assert_eq!(action, BindAction::ViewWs(2)); // 1-indexed -> 0-indexed
    }

    #[test]
    fn test_expand_range() {
        let tokens = &["mod+{1-9}", "view_ws", "{1-9}"];
        let binds = expand_range_binds(tokens).unwrap();
        assert_eq!(binds.len(), 9);
        assert_eq!(binds[0].action, BindAction::ViewWs(0));
        assert_eq!(binds[8].action, BindAction::ViewWs(8));
    }

    #[test]
    fn test_xf86_key() {
        let (mods, _sym) = parse_combo("XF86AudioRaiseVolume").unwrap();
        assert_eq!(mods, ModMask::NONE);
    }
}

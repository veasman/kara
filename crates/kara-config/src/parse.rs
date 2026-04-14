/// Config file parser — hand-rolled block-based format.
///
/// Line-based parser with:
/// - `split_tokens()` tokenizer (handles single/double quotes, backslash escapes)
/// - `strip_comment()` (respects quotes)
/// - `$variable` expansion (preprocessing pass)
/// - `include` with `~` expansion and depth limit 16
/// - Block state machine for nested config blocks
/// - Per-block parse functions
/// - Error messages with file:line context

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::keybind::expand_range_binds;
use crate::types::*;

const INCLUDE_MAX_DEPTH: usize = 16;

// ── Tokenizer ───────────────────────────────────────────────────────

/// Split a line into tokens, respecting single/double quotes and backslash escapes.
/// Port of C `split_line_tokens()`.
fn split_tokens(src: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Skip whitespace
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }

        let mut token = String::new();
        let mut in_single = false;
        let mut in_double = false;

        while i < bytes.len() {
            let ch = bytes[i];

            if !in_single && !in_double && ch.is_ascii_whitespace() {
                break;
            }

            if !in_double && ch == b'\'' {
                in_single = !in_single;
                i += 1;
                continue;
            }

            if !in_single && ch == b'"' {
                in_double = !in_double;
                i += 1;
                continue;
            }

            if ch == b'\\' {
                i += 1;
                if i < bytes.len() {
                    token.push(bytes[i] as char);
                }
                i += 1;
                continue;
            }

            token.push(ch as char);
            i += 1;
        }

        if !token.is_empty() {
            tokens.push(token);
        }
    }

    tokens
}

/// Strip `#` comments from a line, respecting quoted strings.
fn strip_comment(line: &mut String) {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for (i, ch) in line.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == b'\\' {
            escaped = true;
            continue;
        }
        if !in_single && ch == b'"' {
            in_double = !in_double;
            continue;
        }
        if !in_double && ch == b'\'' {
            in_single = !in_single;
            continue;
        }
        if !in_single && !in_double && ch == b'#' {
            line.truncate(i);
            return;
        }
    }
}

/// Expand `~` to `$HOME` in a path string.
fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix('~') {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest.strip_prefix('/').unwrap_or(rest));
        }
    }
    PathBuf::from(path)
}

/// Resolve an include path relative to the including file's directory.
fn resolve_include(base_path: &Path, include_path: &str) -> PathBuf {
    let expanded = expand_home(include_path);
    if expanded.is_absolute() {
        return expanded;
    }
    // Relative to the directory of the base file
    match base_path.parent() {
        Some(dir) => dir.join(expanded),
        None => expanded,
    }
}

// ── Variable expansion ──────────────────────────────────────────────

/// Expand `$variable` references in a line using the variable map.
fn expand_variables(line: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'$' {
            // Read variable name (alphanumeric + underscore)
            let start = i + 1;
            let mut end = start;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
            {
                end += 1;
            }
            if end > start {
                let name = &line[start..end];
                if let Some(val) = vars.get(name) {
                    result.push_str(val);
                } else {
                    // Keep the original $name if not defined
                    result.push_str(&line[i..end]);
                }
                i = end;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }

    result
}

// ── Parse error context ─────────────────────────────────────────────

struct ParseContext {
    path: PathBuf,
    line_num: usize,
}

impl ParseContext {
    fn warn(&self, msg: &str) {
        eprintln!("kara-config: {}:{}: {}", self.path.display(), self.line_num, msg);
    }
}

// ── Block state machine ─────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    None,
    General,
    Theme,
    Animations,
    Bar,
    BarModules,
    Scratchpad(usize),
    Rules,
    Autostart,
    Commands,
    Binds,
    Environment,
    Input,
    InputDevice,
    Monitor(usize),
}

fn root_block_name(name: &str) -> Option<Block> {
    match name {
        "general" => Some(Block::General),
        "theme" => Some(Block::Theme),
        "animations" => Some(Block::Animations),
        "bar" => Some(Block::Bar),
        // scratchpad is handled specially (has a name argument)
        "rules" => Some(Block::Rules),
        "autostart" => Some(Block::Autostart),
        "commands" => Some(Block::Commands),
        "binds" => Some(Block::Binds),
        "environment" | "env" => Some(Block::Environment),
        "input" => Some(Block::Input),
        _ => None,
    }
}

// ── Per-block parsers ───────────────────────────────────────────────

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_color(s: &str) -> Option<u32> {
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    u32::from_str_radix(s, 16).ok().map(|v| v & 0x00ff_ffff)
}

fn parse_general_line(tokens: &[String], general: &mut General, ctx: &ParseContext) {
    if tokens.len() < 2 {
        return;
    }
    let key = tokens[0].as_str();
    let val = tokens[1].as_str();

    match key {
        "font" | "font_family" => general.font = val.to_string(),
        "font_size" => {
            if let Ok(v) = val.parse::<f32>() {
                general.font_size = v;
            }
        }
        "border_px" | "border_width" => {
            if let Ok(v) = val.parse::<i32>() {
                general.border_px = v;
            }
        }
        "border_radius" | "rounding" => {
            if let Ok(v) = val.parse::<i32>() {
                general.border_radius = v;
            }
        }
        "gap_px" => {
            if let Ok(v) = val.parse::<i32>() {
                general.gap_px = v;
            }
        }
        "default_mfact" => {
            if let Ok(v) = val.parse::<f32>() {
                general.default_mfact = v;
            }
        }
        "sync_workspaces" => {
            if let Some(b) = parse_bool(val) {
                general.sync_workspaces = b;
            }
        }
        "cursor_theme" => general.cursor_theme = Some(val.to_string()),
        "cursor_size" => {
            if let Ok(v) = val.parse::<i32>() {
                general.cursor_size = v.max(1);
            }
        }
        _ => ctx.warn(&format!("unknown general key '{key}'")),
    }
}

fn parse_theme_line(tokens: &[String], theme: &mut Theme, ctx: &ParseContext) {
    if tokens.len() < 2 {
        return;
    }
    let key = tokens[0].as_str();
    let val = tokens[1].as_str();

    let color = match parse_color(val) {
        Some(c) => c,
        None => {
            ctx.warn(&format!("invalid theme color '{val}'"));
            return;
        }
    };

    match key {
        "bg" | "background" => theme.bg = color,
        "surface" => theme.surface = color,
        "text" => theme.text = color,
        "text_muted" | "muted" => theme.text_muted = color,
        "accent" => theme.accent = color,
        "accent_soft" => theme.accent_soft = color,
        "border" => theme.border = color,
        _ => ctx.warn(&format!("unknown theme key '{key}'")),
    }
}

fn parse_animations_line(tokens: &[String], anims: &mut Animations, ctx: &ParseContext) {
    if tokens.len() < 2 {
        return;
    }
    let key = tokens[0].as_str();
    let val = tokens[1].as_str();

    match key {
        "preset" => {
            anims.preset = match val {
                "instant" | "none" => AnimationPreset::Instant,
                "clean" => AnimationPreset::Clean,
                "swoosh" => AnimationPreset::Swoosh,
                _ => {
                    ctx.warn(&format!("unknown animation preset '{val}'"));
                    return;
                }
            };
        }
        "duration" => {
            match val.parse::<u32>() {
                Ok(v) => anims.duration_ms = v.clamp(0, 2000),
                Err(_) => ctx.warn(&format!("invalid animation duration '{val}'")),
            }
        }
        // Accept old keys silently for backward compat
        "level" | "window_open" | "window_close" | "workspace_switch"
        | "scratchpad" | "focus_border" => {}
        _ => ctx.warn(&format!("unknown animation key '{key}'")),
    }
}

fn parse_bar_line(tokens: &[String], bar: &mut Bar, ctx: &ParseContext) {
    if tokens.len() < 2 {
        return;
    }
    let key = tokens[0].as_str();
    let val = tokens[1].as_str();

    match key {
        "enabled" => {
            if let Some(b) = parse_bool(val) {
                bar.enabled = b;
            }
        }
        "background" => {
            if let Some(b) = parse_bool(val) {
                bar.background = b;
            }
        }
        "position" => match val {
            "top" => bar.position = BarPosition::Top,
            "bottom" => bar.position = BarPosition::Bottom,
            _ => ctx.warn(&format!("invalid bar position '{val}'")),
        },
        "height" => {
            if let Ok(v) = val.parse() {
                bar.height = v;
            }
        }
        "radius" => {
            if let Ok(v) = val.parse() {
                bar.radius = v;
            }
        }
        "modules" => match val {
            "flat" => bar.module_style = BarModuleStyle::Flat,
            "pill" | "pills" => bar.module_style = BarModuleStyle::Pill,
            _ => ctx.warn(&format!("invalid bar module style '{val}'")),
        },
        "icons" => {
            if let Some(b) = parse_bool(val) {
                bar.icons = b;
            }
        }
        "colors" => {
            if let Some(b) = parse_bool(val) {
                bar.colors = b;
            }
        }
        "minimal" => {
            if let Some(true) = parse_bool(val) {
                bar.icons = false;
                bar.colors = false;
                bar.minimal = true;
            }
        }
        "margin_x" => {
            if let Ok(v) = val.parse() {
                bar.margin_x = v;
            }
        }
        "margin_y" => {
            if let Ok(v) = val.parse() {
                bar.margin_y = v;
            }
        }
        "content_margin_x" => {
            if let Ok(v) = val.parse() {
                bar.content_margin_x = v;
            }
        }
        "content_margin_y" => {
            if let Ok(v) = val.parse() {
                bar.content_margin_y = v;
            }
        }
        "gap" => {
            if let Ok(v) = val.parse() {
                bar.gap = v;
            }
        }
        "padding_x" => {
            if let Ok(v) = val.parse() {
                bar.padding_x = v;
            }
        }
        "padding_y" => {
            if let Ok(v) = val.parse() {
                bar.padding_y = v;
            }
        }
        "volume_bar_enabled" => {
            if let Some(b) = parse_bool(val) {
                bar.volume_bar_enabled = b;
            }
        }
        "volume_bar_width" => {
            if let Ok(v) = val.parse() {
                bar.volume_bar_width = v;
            }
        }
        "volume_bar_height" => {
            if let Ok(v) = val.parse() {
                bar.volume_bar_height = v;
            }
        }
        "volume_bar_radius" => {
            if let Ok(v) = val.parse() {
                bar.volume_bar_radius = v;
            }
        }
        _ => ctx.warn(&format!("unknown bar key '{key}'")),
    }
}

fn parse_bar_module_kind(name: &str) -> Option<BarModuleKind> {
    match name {
        "workspaces" => Some(BarModuleKind::Workspaces),
        "monitor" => Some(BarModuleKind::Monitor),
        "sync" => Some(BarModuleKind::Sync),
        "title" => Some(BarModuleKind::Title),
        "status" => Some(BarModuleKind::Status),
        "clock" => Some(BarModuleKind::Clock),
        "custom" => Some(BarModuleKind::Custom),
        "volume" => Some(BarModuleKind::Volume),
        "network" | "wifi" => Some(BarModuleKind::Network),
        "battery" => Some(BarModuleKind::Battery),
        "brightness" => Some(BarModuleKind::Brightness),
        "media" => Some(BarModuleKind::Media),
        "memory" | "ram" => Some(BarModuleKind::Memory),
        "cpu" => Some(BarModuleKind::Cpu),
        "weather" => Some(BarModuleKind::Weather),
        _ => {
            if let Some(script_name) = name.strip_prefix("script:") {
                if !script_name.is_empty() {
                    return Some(BarModuleKind::Script(script_name.to_string()));
                }
            }
            None
        }
    }
}

fn parse_bar_modules_line(tokens: &[String], bar: &mut Bar, ctx: &ParseContext) {
    if tokens.len() < 2 {
        return;
    }

    let section = match tokens[0].as_str() {
        "left" => BarSection::Left,
        "center" => BarSection::Center,
        "right" => BarSection::Right,
        _ => {
            ctx.warn(&format!("invalid modules section '{}'", tokens[0]));
            return;
        }
    };

    let kind = match parse_bar_module_kind(&tokens[1]) {
        Some(k) => k,
        None => {
            ctx.warn(&format!("unknown bar module '{}'", tokens[1]));
            return;
        }
    };

    let arg = tokens.get(2).cloned();

    bar.modules.push(BarModule { section, kind, arg });
}

fn parse_scratchpad_line(tokens: &[String], scratch: &mut ScratchpadConfig, ctx: &ParseContext) {
    if tokens.len() < 2 {
        return;
    }
    let key = tokens[0].as_str();
    let val = &tokens[1];

    match key {
        "width_pct" => {
            if let Ok(v) = val.parse() {
                scratch.width_pct = v;
            }
        }
        "height_pct" => {
            if let Ok(v) = val.parse() {
                scratch.height_pct = v;
            }
        }
        "dim_alpha" => {
            if let Ok(v) = val.parse() {
                scratch.dim_alpha = v;
            }
        }
        "blur" => scratch.blur = parse_bool(val).unwrap_or(false),
        "overlay" => scratch.overlay = Some(val.clone()),
        "autostart" => scratch.autostart = Some(val.clone()),
        "capture" => {
            if tokens.len() >= 3 && val == "app_id" {
                scratch.captures.push(tokens[2].clone());
            } else {
                ctx.warn("capture requires: capture app_id \"pattern\"");
            }
        }
        _ => ctx.warn(&format!("unknown scratchpad key '{key}'")),
    }
}

fn parse_rules_line(tokens: &[String], rules: &mut Vec<Rule>, ctx: &ParseContext) {
    if tokens.len() < 3 {
        return;
    }

    match tokens[0].as_str() {
        "float" => {
            if tokens[1] == "app_id" {
                rules.push(Rule::Float { app_id: tokens[2].clone() });
            } else {
                ctx.warn(&format!("expected 'app_id' after float, got '{}'", tokens[1]));
            }
        }
        "workspace" => {
            if tokens.len() >= 4 && tokens[2] == "app_id" {
                let ws: usize = match tokens[1].parse::<usize>() {
                    Ok(n) if (1..=9).contains(&n) => n - 1,
                    _ => {
                        ctx.warn(&format!("invalid workspace number '{}'", tokens[1]));
                        return;
                    }
                };
                let mut monitor = None;
                if tokens.len() >= 6 && tokens[4] == "monitor" {
                    if let Ok(m) = tokens[5].parse::<usize>() {
                        monitor = Some(m - 1);
                    }
                }
                rules.push(Rule::Workspace {
                    workspace: ws,
                    app_id: tokens[3].clone(),
                    monitor,
                });
            } else {
                ctx.warn("workspace rule requires: workspace N app_id \"name\"");
            }
        }
        _ => ctx.warn(&format!("unknown rule directive '{}'", tokens[0])),
    }
}

fn parse_autostart_line(tokens: &[String], entries: &mut Vec<AutostartEntry>, ctx: &ParseContext) {
    if tokens.len() < 2 || tokens[0] != "run" {
        if !tokens.is_empty() {
            ctx.warn(&format!("unknown autostart directive '{}'", tokens[0]));
        }
        return;
    }

    let command = tokens[1].clone();
    let mut app_id = None;
    let mut workspace = None;
    let mut monitor = None;

    let mut i = 2;
    while i + 1 < tokens.len() {
        match tokens[i].as_str() {
            "app_id" => app_id = Some(tokens[i + 1].clone()),
            "workspace" => workspace = tokens[i + 1].parse::<usize>().ok().map(|n| n - 1),
            "monitor" => monitor = tokens[i + 1].parse::<usize>().ok().map(|n| n - 1),
            _ => ctx.warn(&format!("unknown autostart option '{}'", tokens[i])),
        }
        i += 2;
    }

    entries.push(AutostartEntry { command, app_id, workspace, monitor });
}

fn parse_commands_line(tokens: &[String], commands: &mut HashMap<String, String>) {
    if tokens.len() >= 2 {
        commands.insert(tokens[0].clone(), tokens[1].clone());
    }
}

fn parse_binds_line(
    tokens: &[String],
    keybinds: &mut Vec<Keybind>,
    ctx: &ParseContext,
) {
    if tokens.len() < 2 {
        return;
    }

    // Skip optional "bind" prefix
    let start = if tokens[0] == "bind" { 1 } else { 0 };
    let slice: Vec<&str> = tokens[start..].iter().map(|s| s.as_str()).collect();

    match expand_range_binds(&slice) {
        Ok(binds) => keybinds.extend(binds),
        Err(e) => ctx.warn(&e),
    }
}

fn parse_environment_line(tokens: &[String], env: &mut Vec<EnvDirective>, ctx: &ParseContext) {
    if tokens.is_empty() {
        return;
    }

    match tokens[0].as_str() {
        "env" => {
            if tokens.len() >= 3 {
                env.push(EnvDirective::Set {
                    key: tokens[1].clone(),
                    value: tokens[2].clone(),
                });
            } else {
                ctx.warn("env requires KEY and VALUE");
            }
        }
        "source" => {
            if tokens.len() >= 2 {
                env.push(EnvDirective::Source {
                    path: tokens[1].clone(),
                });
            } else {
                ctx.warn("source requires a path");
            }
        }
        _ => ctx.warn(&format!("unknown environment directive '{}'", tokens[0])),
    }
}

fn parse_monitor_line(tokens: &[String], mon: &mut MonitorConfig, _ctx: &ParseContext) {
    // Bare-keyword shortcut for `primary` — `primary` and `primary true` are
    // both accepted; `primary false` explicitly clears.
    if tokens[0] == "primary" {
        mon.primary = if tokens.len() >= 2 {
            matches!(tokens[1].as_str(), "true" | "yes" | "1")
        } else {
            true
        };
        return;
    }
    if tokens.len() < 2 { return; }
    match tokens[0].as_str() {
        "resolution" | "mode" | "res" => {
            let s = &tokens[1];
            if let Some((w, h)) = s.split_once('x')
                .and_then(|(w, h)| Some((w.parse::<i32>().ok()?, h.parse::<i32>().ok()?)))
            {
                mon.resolution = Some((w, h));
            } else if tokens.len() >= 3 {
                if let (Ok(w), Ok(h)) = (tokens[1].parse::<i32>(), tokens[2].parse::<i32>()) {
                    mon.resolution = Some((w, h));
                }
            }
        }
        "refresh" | "hz" => {
            if let Ok(v) = tokens[1].parse::<u32>() {
                mon.refresh = Some(v);
            }
        }
        "position" | "pos" => {
            let s = &tokens[1];
            if let Some((x, y)) = s.split_once(',')
                .or_else(|| s.split_once('x'))
                .and_then(|(x, y)| Some((x.parse::<i32>().ok()?, y.parse::<i32>().ok()?)))
            {
                mon.position = Some((x, y));
            } else if tokens.len() >= 3 {
                if let (Ok(x), Ok(y)) = (tokens[1].parse::<i32>(), tokens[2].parse::<i32>()) {
                    mon.position = Some((x, y));
                }
            }
        }
        "scale" => {
            if let Ok(v) = tokens[1].parse::<f64>() {
                mon.scale = Some(v);
            }
        }
        "enabled" | "enable" => {
            mon.enabled = matches!(tokens[1].as_str(), "true" | "yes" | "1");
        }
        "disabled" | "disable" => {
            mon.enabled = !matches!(tokens[1].as_str(), "true" | "yes" | "1");
        }
        "rotate" | "rotation" | "transform" => {
            mon.rotation = match tokens[1].as_str() {
                "left" | "90" | "ccw" => MonitorRotation::Left,
                "right" | "270" | "cw" => MonitorRotation::Right,
                "flipped" | "180" | "inverted" => MonitorRotation::Flipped,
                _ => MonitorRotation::Normal,
            };
        }
        _ => {}
    }
}

fn parse_input_device_line(
    tokens: &[String],
    device: &mut InputDevice,
    ctx: &ParseContext,
) {
    if tokens.len() < 2 {
        return;
    }
    let key = tokens[0].as_str();
    let val = tokens[1].as_str();

    match key {
        "accel_profile" => {
            device.accel_profile = match val {
                "flat" => Some(AccelProfile::Flat),
                "adaptive" => Some(AccelProfile::Adaptive),
                _ => {
                    ctx.warn(&format!("unknown accel_profile '{val}'"));
                    None
                }
            };
        }
        "accel_speed" => {
            if let Ok(v) = val.parse::<f64>() {
                device.accel_speed = Some(v.clamp(-1.0, 1.0));
            }
        }
        "natural_scroll" => {
            if let Some(b) = parse_bool(val) {
                device.natural_scroll = Some(b);
            }
        }
        "tap_to_click" | "tap" => {
            if let Some(b) = parse_bool(val) {
                device.tap_to_click = Some(b);
            }
        }
        "tap_and_drag" => {
            if let Some(b) = parse_bool(val) {
                device.tap_and_drag = Some(b);
            }
        }
        "dwt" | "disable_while_typing" => {
            if let Some(b) = parse_bool(val) {
                device.dwt = Some(b);
            }
        }
        "scroll_method" => device.scroll_method = Some(val.to_string()),
        "click_method" => device.click_method = Some(val.to_string()),
        "left_handed" => {
            if let Some(b) = parse_bool(val) {
                device.left_handed = Some(b);
            }
        }
        "middle_emulation" => {
            if let Some(b) = parse_bool(val) {
                device.middle_emulation = Some(b);
            }
        }
        _ => ctx.warn(&format!("unknown input key '{key}'")),
    }
}

// ── Sanitize ────────────────────────────────────────────────────────

fn sanitize(config: &mut Config) {
    let g = &mut config.general;
    g.border_px = g.border_px.max(0);
    g.border_radius = g.border_radius.max(0);
    g.gap_px = g.gap_px.max(0);
    g.default_mfact = g.default_mfact.clamp(0.05, 0.95);
    if g.font_size <= 0.0 {
        g.font_size = 11.0;
    }

    let b = &mut config.bar;
    b.height = b.height.max(18);
    b.gap = b.gap.max(0);
    b.padding_x = b.padding_x.max(0);
    b.padding_y = b.padding_y.max(0);
    b.radius = b.radius.max(0);
    b.margin_x = b.margin_x.max(0);
    b.margin_y = b.margin_y.max(0);
    b.content_margin_x = b.content_margin_x.max(0);
    b.content_margin_y = b.content_margin_y.max(0);
    b.volume_bar_width = b.volume_bar_width.max(0);
    b.volume_bar_height = b.volume_bar_height.max(0);
    b.volume_bar_radius = b.volume_bar_radius.max(0);

    for s in &mut config.scratchpads {
        s.width_pct = s.width_pct.clamp(40, 100);
        s.height_pct = s.height_pct.clamp(40, 100);
        s.dim_alpha = s.dim_alpha.clamp(0, 255);
    }
}

// ── Main parser ─────────────────────────────────────────────────────

/// Load and parse a config file, returning a fully populated Config.
pub fn load_config(path: &Path) -> Config {
    let mut config = Config::default();
    let mut vars = HashMap::new();
    load_file_recursive(path, 0, &mut config, &mut vars);
    sanitize(&mut config);
    config
}

/// Load the default config from `~/.config/kara/kara-gate.conf`.
pub fn load_default_config() -> Config {
    let path = default_config_path();
    if path.exists() {
        load_config(&path)
    } else {
        Config::default()
    }
}

/// Return the default config file path.
pub fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("kara")
        .join("kara-gate.conf")
}

fn load_file_recursive(
    path: &Path,
    depth: usize,
    config: &mut Config,
    vars: &mut HashMap<String, String>,
) {
    if depth > INCLUDE_MAX_DEPTH {
        eprintln!("kara-config: include depth exceeded while loading {}", path.display());
        return;
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kara-config: could not open {}: {e}", path.display());
            return;
        }
    };

    let mut block = Block::None;
    let mut parent_block = Block::None;

    let mut ctx = ParseContext {
        path: path.to_path_buf(),
        line_num: 0,
    };

    for (line_idx, raw_line) in content.lines().enumerate() {
        ctx.line_num = line_idx + 1;

        let mut line = raw_line.to_string();
        strip_comment(&mut line);

        // Variable expansion
        line = expand_variables(&line, vars);

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Variable definition: $name = value
        if trimmed.starts_with('$') {
            if let Some((name, value)) = parse_variable_def(trimmed) {
                vars.insert(name, value);
            } else {
                ctx.warn("invalid variable definition");
            }
            continue;
        }

        // Close brace
        if trimmed == "}" {
            if parent_block != Block::None {
                block = parent_block;
                parent_block = Block::None;
            } else {
                block = Block::None;
            }
            continue;
        }

        // Include directive (only at top level)
        if block == Block::None {
            let tokens = split_tokens(trimmed);
            if tokens.len() >= 2 && tokens[0] == "include" {
                let include_path = resolve_include(path, &tokens[1]);
                if include_path == path {
                    ctx.warn("skipping self-include");
                    continue;
                }
                load_file_recursive(&include_path, depth + 1, config, vars);
                continue;
            }
        }

        // Open brace — enter a block
        if trimmed.ends_with('{') {
            let name = trimmed[..trimmed.len() - 1].trim();

            if block == Block::None {
                // Check for scratchpad "name" { } — named block with argument
                let block_tokens = split_tokens(name);
                if block_tokens.first().map(|s| s.as_str()) == Some("scratchpad") {
                    let sp_name = block_tokens.get(1).cloned().unwrap_or_else(|| "main".to_string());
                    config.scratchpads.push(ScratchpadConfig::new(&sp_name));
                    block = Block::Scratchpad(config.scratchpads.len() - 1);
                    continue;
                }

                if block_tokens.first().map(|s| s.as_str()) == Some("monitor") {
                    let mon_name = block_tokens.get(1).map(|s| s.trim_matches('"').to_string())
                        .unwrap_or_default();
                    config.monitors.push(MonitorConfig {
                        name: mon_name,
                        resolution: None,
                        refresh: None,
                        position: None,
                        scale: None,
                        rotation: MonitorRotation::Normal,
                        enabled: true,
                        primary: false,
                    });
                    let idx = config.monitors.len() - 1;
                    block = Block::Monitor(idx);
                    continue;
                }

                match root_block_name(name) {
                    Some(b) => {
                        block = b;
                    }
                    None => ctx.warn(&format!("unknown block '{name}'")),
                }
                continue;
            }

            // Nested block
            if parent_block != Block::None {
                ctx.warn("nested block depth > 1 is not supported");
                continue;
            }

            if block == Block::Bar && name == "modules" {
                config.bar.modules.clear();
                parent_block = block;
                block = Block::BarModules;
                continue;
            }

            // input { device "name" { ... } } — or just input { ... } for defaults
            if block == Block::Input {
                let tokens = split_tokens(name);
                if tokens.first().map(|s| s.as_str()) == Some("device") {
                    let dev_name = tokens.get(1).cloned();
                    config.input.push(InputDevice {
                        device: dev_name,
                        accel_profile: None,
                        accel_speed: None,
                        natural_scroll: None,
                        tap_to_click: None,
                        tap_and_drag: None,
                        dwt: None,
                        scroll_method: None,
                        click_method: None,
                        left_handed: None,
                        middle_emulation: None,
                    });
                    parent_block = block;
                    block = Block::InputDevice;
                    continue;
                }
            }

            ctx.warn(&format!("nested block '{name}' is not valid here"));
            continue;
        }

        // Directive outside any block
        if block == Block::None {
            ctx.warn(&format!("directive outside block: {trimmed}"));
            continue;
        }

        // Parse line within current block
        let tokens = split_tokens(trimmed);
        if tokens.is_empty() {
            continue;
        }

        match block {
            Block::General => parse_general_line(&tokens, &mut config.general, &ctx),
            Block::Theme => parse_theme_line(&tokens, &mut config.theme, &ctx),
            Block::Animations => parse_animations_line(&tokens, &mut config.animations, &ctx),
            Block::Bar => parse_bar_line(&tokens, &mut config.bar, &ctx),
            Block::BarModules => parse_bar_modules_line(&tokens, &mut config.bar, &ctx),
            Block::Scratchpad(idx) => {
                if let Some(sp) = config.scratchpads.get_mut(idx) {
                    parse_scratchpad_line(&tokens, sp, &ctx);
                }
            }
            Block::Rules => parse_rules_line(&tokens, &mut config.rules, &ctx),
            Block::Autostart => parse_autostart_line(&tokens, &mut config.autostart, &ctx),
            Block::Commands => parse_commands_line(&tokens, &mut config.commands),
            Block::Binds => parse_binds_line(&tokens, &mut config.keybinds, &ctx),
            Block::Environment => parse_environment_line(&tokens, &mut config.environment, &ctx),
            Block::Input => {
                // Top-level input settings apply as a default device (no name)
                if config.input.is_empty() || config.input.last().unwrap().device.is_some() {
                    config.input.push(InputDevice {
                        device: None,
                        accel_profile: None, accel_speed: None,
                        natural_scroll: None, tap_to_click: None,
                        tap_and_drag: None, dwt: None,
                        scroll_method: None, click_method: None,
                        left_handed: None, middle_emulation: None,
                    });
                }
                let last = config.input.last_mut().unwrap();
                parse_input_device_line(&tokens, last, &ctx);
            }
            Block::InputDevice => {
                if let Some(dev) = config.input.last_mut() {
                    parse_input_device_line(&tokens, dev, &ctx);
                }
            }
            Block::Monitor(idx) => {
                if let Some(mon) = config.monitors.get_mut(idx) {
                    parse_monitor_line(&tokens, mon, &ctx);
                }
            }
            Block::None => unreachable!(),
        }
    }
}

/// Parse `$name = value` into (name, value).
fn parse_variable_def(line: &str) -> Option<(String, String)> {
    // line starts with $
    let rest = &line[1..];
    let eq_pos = rest.find('=')?;
    let name = rest[..eq_pos].trim().to_string();
    let value = rest[eq_pos + 1..].trim().to_string();

    if name.is_empty() || value.is_empty() {
        return None;
    }

    // Strip quotes from value if present
    let value = if (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value
    };

    Some((name, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_tokens_basic() {
        let tokens = split_tokens("font_size 10");
        assert_eq!(tokens, vec!["font_size", "10"]);
    }

    #[test]
    fn test_split_tokens_quoted() {
        let tokens = split_tokens(r#"font "FiraCode Nerd Font""#);
        assert_eq!(tokens, vec!["font", "FiraCode Nerd Font"]);
    }

    #[test]
    fn test_split_tokens_single_quoted() {
        let tokens = split_tokens("font 'Mono Bold'");
        assert_eq!(tokens, vec!["font", "Mono Bold"]);
    }

    #[test]
    fn test_split_tokens_backslash() {
        let tokens = split_tokens(r"path /foo\ bar");
        assert_eq!(tokens, vec!["path", "/foo bar"]);
    }

    #[test]
    fn test_strip_comment() {
        let mut line = "font_size 10 # comment".to_string();
        strip_comment(&mut line);
        assert_eq!(line, "font_size 10 ");
    }

    #[test]
    fn test_strip_comment_in_quotes() {
        let mut line = r#"clock "%a %d #%b" # real comment"#.to_string();
        strip_comment(&mut line);
        assert_eq!(line, r#"clock "%a %d #%b" "#);
    }

    #[test]
    fn test_variable_expansion() {
        let mut vars = HashMap::new();
        vars.insert("terminal".into(), "kitty".into());
        let result = expand_variables("spawn $terminal", &vars);
        assert_eq!(result, "spawn kitty");
    }

    #[test]
    fn test_variable_def() {
        let (name, val) = parse_variable_def("$terminal = kitty").unwrap();
        assert_eq!(name, "terminal");
        assert_eq!(val, "kitty");
    }

    #[test]
    fn test_variable_def_quoted() {
        let (name, val) = parse_variable_def("$launcher = \"rofi -show drun\"").unwrap();
        assert_eq!(name, "launcher");
        assert_eq!(val, "rofi -show drun");
    }

    #[test]
    fn test_parse_color() {
        assert_eq!(parse_color("0x111111"), Some(0x111111));
        assert_eq!(parse_color("0xFF00FF"), Some(0xFF00FF));
    }

    #[test]
    fn test_parse_bool() {
        assert_eq!(parse_bool("true"), Some(true));
        assert_eq!(parse_bool("false"), Some(false));
        assert_eq!(parse_bool("1"), Some(true));
        assert_eq!(parse_bool("0"), Some(false));
        assert_eq!(parse_bool("bogus"), None);
    }

    #[test]
    fn test_expand_home() {
        let path = expand_home("~/.config/kara/kara-gate.conf");
        // Should start with the user's home dir, not ~
        assert!(!path.to_str().unwrap().starts_with('~'));
    }
}

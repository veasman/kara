/// Module text building — produces display text, icon, and color for each module.

use kara_config::{BarModuleKind, Theme};

use crate::status::StatusCache;

/// The output of building a module's display content.
pub struct ModuleContent {
    pub text: String,
    pub color: u32,
}

/// Context needed to build module text.
pub struct ModuleContext<'a> {
    pub theme: &'a Theme,
    pub icons: bool,
    pub colors: bool,
    pub status: &'a StatusCache,
    // Compositor-provided state
    pub current_ws: usize,
    pub occupied_workspaces: [bool; 9],
    pub focused_title: String,
    pub monitor_id: usize,
    pub sync_enabled: bool,
    pub is_focused_monitor: bool,
}

pub fn build_module_text(kind: &BarModuleKind, arg: Option<&str>, ctx: &ModuleContext) -> ModuleContent {
    match kind {
        BarModuleKind::Clock => build_clock(arg, ctx),
        BarModuleKind::Volume => build_volume(ctx),
        BarModuleKind::Network => build_network(ctx),
        BarModuleKind::Battery => build_battery(ctx),
        BarModuleKind::Brightness => build_brightness(ctx),
        BarModuleKind::Media => build_media(ctx),
        BarModuleKind::Memory => build_memory(ctx),
        BarModuleKind::Cpu => build_cpu(ctx),
        BarModuleKind::Title => build_title(ctx),
        BarModuleKind::Monitor => build_monitor(ctx),
        BarModuleKind::Sync => build_sync(ctx),
        BarModuleKind::Weather => build_weather(ctx),
        BarModuleKind::Status => build_status(ctx),
        BarModuleKind::Custom => build_custom(arg, ctx),
        BarModuleKind::Script(name) => build_script(name, ctx),
        // Workspaces is handled specially in the renderer, not as text
        BarModuleKind::Workspaces => ModuleContent {
            text: String::new(),
            color: ctx.theme.text,
        },
    }
}

fn build_clock(arg: Option<&str>, ctx: &ModuleContext) -> ModuleContent {
    let fmt = arg.unwrap_or("%a %b %d \u{2022} %H:%M"); // bullet separator
    let now = chrono::Local::now();
    let text = now.format(fmt).to_string();
    let display = if ctx.icons {
        format!("\u{f0954} {text}") // 󰥔 clock icon
    } else {
        text
    };
    ModuleContent { text: display, color: ctx.theme.text }
}

fn build_volume(ctx: &ModuleContext) -> ModuleContent {
    let st = &ctx.status.volume;
    if !st.valid {
        let text = if ctx.icons { "\u{f0581} audio" } else { "audio" };
        return ModuleContent { text: text.into(), color: ctx.theme.text_muted };
    }

    let icon = if st.muted {
        "\u{f075f}" // 󰝟 muted
    } else if st.percent >= 66 {
        "\u{f057e}" // 󰕾 high
    } else if st.percent >= 33 {
        "\u{f0580}" // 󰖀 medium
    } else {
        "\u{f057f}" // 󰕿 low
    };

    let text = if st.muted {
        if ctx.icons { format!("{icon} muted") } else { "vol muted".into() }
    } else if ctx.icons {
        format!("{icon} {}%", st.percent)
    } else {
        format!("vol {}%", st.percent)
    };

    let color = if !ctx.colors {
        ctx.theme.text
    } else if st.muted {
        ctx.theme.text_muted
    } else {
        ctx.theme.text
    };

    ModuleContent { text, color }
}

fn build_network(ctx: &ModuleContext) -> ModuleContent {
    let st = &ctx.status.network;
    if !st.valid || !st.connected {
        let text = if ctx.icons { "\u{f0a8e} offline" } else { "offline" };
        let color = if ctx.colors { ctx.theme.text_muted } else { ctx.theme.text };
        return ModuleContent { text: text.into(), color };
    }

    let text = if st.wireless {
        let signal = if st.signal_percent >= 0 {
            format!(" {}%", st.signal_percent)
        } else {
            String::new()
        };
        if ctx.icons {
            format!("\u{f05a9}{signal}") // 󰖩 wifi icon
        } else if st.ssid.is_empty() {
            format!("wifi{signal}")
        } else {
            format!("{}{signal}", st.ssid)
        }
    } else if ctx.icons {
        "\u{f0200}".into() // 󰈀 ethernet icon
    } else {
        "eth".into()
    };

    let color = if ctx.colors { ctx.theme.accent } else { ctx.theme.text };
    ModuleContent { text, color }
}

fn build_battery(ctx: &ModuleContext) -> ModuleContent {
    let st = &ctx.status.battery;
    if !st.valid {
        return ModuleContent { text: String::new(), color: ctx.theme.text };
    }

    let icon = if st.full || st.percent >= 95 {
        "\u{f0079}" // 󰁹 full
    } else if st.charging {
        "\u{f0084}" // 󰂄 charging
    } else if st.percent >= 70 {
        "\u{f0082}" // 󰂂
    } else if st.percent >= 40 {
        "\u{f007e}" // 󰁾
    } else if st.percent >= 15 {
        "\u{f007a}" // 󰁺
    } else {
        "\u{f008e}" // 󰂎 critical
    };

    let text = if ctx.icons {
        format!("{icon} {}%", st.percent)
    } else {
        format!("bat {}%", st.percent)
    };

    let color = if !ctx.colors {
        ctx.theme.text
    } else if st.full {
        ctx.theme.accent
    } else if st.charging {
        ctx.theme.accent_soft
    } else if st.percent <= 15 {
        ctx.theme.accent // using accent for critical (could be a warning color)
    } else {
        ctx.theme.text
    };

    ModuleContent { text, color }
}

fn build_brightness(ctx: &ModuleContext) -> ModuleContent {
    let st = &ctx.status.brightness;
    if !st.valid {
        return ModuleContent { text: String::new(), color: ctx.theme.text };
    }

    let text = if ctx.icons {
        format!("\u{f00df} {}%", st.percent) // 󰃟 brightness icon
    } else {
        format!("bri {}%", st.percent)
    };

    ModuleContent { text, color: ctx.theme.text }
}

fn build_media(ctx: &ModuleContext) -> ModuleContent {
    let st = &ctx.status.media;
    if !st.valid || st.text.is_empty() {
        return ModuleContent { text: String::new(), color: ctx.theme.text };
    }

    let icon = if st.playing {
        "\u{f040a}" // 󰐊 play
    } else if st.paused {
        "\u{f03e4}" // 󰏤 pause
    } else {
        "\u{f04db}" // 󰓛 stop
    };

    let text = if ctx.icons {
        format!("{icon} {}", st.text)
    } else {
        st.text.clone()
    };

    let color = if ctx.colors && st.playing {
        ctx.theme.accent
    } else {
        ctx.theme.text
    };

    ModuleContent { text, color }
}

fn build_memory(ctx: &ModuleContext) -> ModuleContent {
    let st = &ctx.status.memory;
    if !st.valid {
        return ModuleContent { text: String::new(), color: ctx.theme.text };
    }

    let text = if ctx.icons {
        format!("\u{f035b} {}%", st.used_percent) // 󰍛 memory icon
    } else {
        format!("mem {}%", st.used_percent)
    };

    let color = if ctx.colors && st.used_percent >= 90 {
        ctx.theme.accent
    } else {
        ctx.theme.text
    };

    ModuleContent { text, color }
}

fn build_cpu(ctx: &ModuleContext) -> ModuleContent {
    let st = &ctx.status.cpu;
    if !st.valid {
        return ModuleContent { text: String::new(), color: ctx.theme.text };
    }

    let text = if ctx.icons {
        format!("\u{f4bc} {}%", st.usage_percent) // 󰒼 cpu/chip icon
    } else {
        format!("cpu {}%", st.usage_percent)
    };

    let color = if ctx.colors && st.usage_percent >= 90 {
        ctx.theme.accent
    } else {
        ctx.theme.text
    };

    ModuleContent { text, color }
}

fn build_title(ctx: &ModuleContext) -> ModuleContent {
    let text = if ctx.focused_title.is_empty() {
        "kara".to_string()
    } else {
        // Truncate long titles
        if ctx.focused_title.len() > 60 {
            format!("{}...", &ctx.focused_title[..57])
        } else {
            ctx.focused_title.clone()
        }
    };

    ModuleContent { text, color: ctx.theme.text }
}

fn build_monitor(ctx: &ModuleContext) -> ModuleContent {
    // Compact form. The number is wrapped in brackets when this monitor has
    // keyboard focus, plain otherwise. Color carries the same signal in
    // case the bracket is too subtle. Sync state is appended after.
    let n = ctx.monitor_id + 1;
    let body = if ctx.is_focused_monitor {
        format!("[{n}]")
    } else {
        format!(" {n} ")
    };

    let text = if ctx.icons {
        if ctx.sync_enabled {
            format!("\u{f0379}{body}\u{f0b38}") // 󰍹 mon  󰬸 sync
        } else {
            format!("\u{f0379}{body}")
        }
    } else if ctx.sync_enabled {
        format!("mon{body}sync")
    } else {
        format!("mon{body}")
    };

    let color = if !ctx.colors {
        ctx.theme.text_muted
    } else if ctx.is_focused_monitor {
        ctx.theme.accent
    } else if ctx.sync_enabled {
        ctx.theme.accent_soft
    } else {
        ctx.theme.text_muted
    };

    ModuleContent { text, color }
}

fn build_sync(ctx: &ModuleContext) -> ModuleContent {
    // Sync is now part of monitor module — return empty for standalone use
    build_monitor(ctx)
}

fn build_weather(ctx: &ModuleContext) -> ModuleContent {
    // Weather is typically provided by a custom command via KARA_WEATHER_CMD
    // For now, return empty — will be populated by script system
    ModuleContent { text: String::new(), color: ctx.theme.text }
}

fn build_status(ctx: &ModuleContext) -> ModuleContent {
    // Status module shows arbitrary text set via IPC (future)
    ModuleContent { text: String::new(), color: ctx.theme.text }
}

fn build_custom(arg: Option<&str>, ctx: &ModuleContext) -> ModuleContent {
    let cmd = match arg {
        Some(c) if !c.is_empty() => c,
        _ => return ModuleContent { text: String::new(), color: ctx.theme.text },
    };

    let output = std::process::Command::new("sh")
        .args(["-c", cmd])
        .output();

    let text = match output {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        _ => String::new(),
    };

    ModuleContent { text, color: ctx.theme.text }
}

fn build_script(name: &str, ctx: &ModuleContext) -> ModuleContent {
    // Script modules run KARA_<NAME>_CMD env var with caching
    // For now, try the env var directly
    let env_name = format!("KARA_{}_CMD", name.to_ascii_uppercase());
    let cmd = match std::env::var(&env_name) {
        Ok(c) if !c.is_empty() => c,
        _ => return ModuleContent { text: String::new(), color: ctx.theme.text },
    };

    let output = std::process::Command::new("sh")
        .args(["-c", &cmd])
        .output();

    let text = match output {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        _ => String::new(),
    };

    ModuleContent { text, color: ctx.theme.text }
}

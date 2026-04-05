use std::io::Write;
use kara_config::*;

/// Parse the full example config from the plan and verify all fields.
#[test]
fn parse_full_example_config() {
    let config_text = r#"
# kara configuration

$terminal = kitty
$launcher = rofi -show drun

general {
    font "FiraCode Nerd Font"
    font_size 10
    border_px 2
    gap_px 8
    default_mfact 0.5
    sync_workspaces true
}

theme {
    bg 0x111111
    surface 0x1b1b1b
    text 0xf2f2f2
    text_muted 0x5c5c5c
    accent 0x6bacac
    accent_soft 0x458588
    border 0x353535
}

animations {
    level light
    window_open 150 ease-out
    window_close 100 ease-out
    workspace_switch 200 ease-in-out
}

bar {
    enabled true
    position top
    height 24
    modules flat

    modules {
        left workspaces
        center title
        right volume
        right clock "%a %d %b %H:%M"
    }
}

scratchpad {
    width_pct 94
    height_pct 94
    dim_alpha 96
    autostart "kitty -e htop"
    define "term" "$terminal" app_id "kitty"
}

rules {
    float app_id "pavucontrol"
    float app_id "mpv"
    workspace 8 app_id "thunderbird"
}

autostart {
    run "dunst"
    run "thunderbird"
}

commands {
    browser "floorp"
    launcher "$launcher"
    terminal "$terminal"
    vol-up "wpctl set-volume -l 1.5 @DEFAULT_AUDIO_SINK@ 5%+"
}

binds {
    "mod+Return" spawn "terminal"
    "mod+d" spawn "launcher"
    "mod+j" focus_next
    "mod+k" focus_prev
    "mod+q" kill_client
    "mod+Shift+q" quit
    "mod+Shift+r" reload
    "mod+f" monocle
    "mod+bracketleft" decrease_mfact
    "mod+bracketright" increase_mfact
    "mod+Shift+Return" zoom_master
    "mod+{1-9}" view_ws {1-9}
    "mod+Shift+{1-9}" send_ws {1-9}
    "XF86AudioRaiseVolume" spawn "vol-up"
}
"#;

    // Write to a temp file
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kara-gate.conf");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(config_text.as_bytes()).unwrap();
    drop(f);

    let config = load_config(&path);

    // General
    assert_eq!(config.general.font, "FiraCode Nerd Font");
    assert_eq!(config.general.font_size, 10.0);
    assert_eq!(config.general.border_px, 2);
    assert_eq!(config.general.gap_px, 8);
    assert!((config.general.default_mfact - 0.5).abs() < f32::EPSILON);
    assert!(config.general.sync_workspaces);

    // Theme
    assert_eq!(config.theme.bg, 0x111111);
    assert_eq!(config.theme.accent, 0x6bacac);
    assert_eq!(config.theme.text_muted, 0x5c5c5c);

    // Animations
    assert_eq!(config.animations.level, AnimationLevel::Light);
    let wo = config.animations.window_open.unwrap();
    assert_eq!(wo.duration_ms, 150);
    assert_eq!(wo.easing, Easing::EaseOut);
    let ws = config.animations.workspace_switch.unwrap();
    assert_eq!(ws.duration_ms, 200);
    assert_eq!(ws.easing, Easing::EaseInOut);

    // Bar
    assert!(config.bar.enabled);
    assert_eq!(config.bar.position, BarPosition::Top);
    assert_eq!(config.bar.height, 24);
    assert_eq!(config.bar.module_style, BarModuleStyle::Flat);
    assert_eq!(config.bar.modules.len(), 4);
    assert_eq!(config.bar.modules[0].section, BarSection::Left);
    assert_eq!(config.bar.modules[0].kind, BarModuleKind::Workspaces);
    assert_eq!(config.bar.modules[3].kind, BarModuleKind::Clock);
    assert_eq!(config.bar.modules[3].arg.as_deref(), Some("%a %d %b %H:%M"));

    // Scratchpad
    assert_eq!(config.scratchpad.width_pct, 94);
    assert_eq!(config.scratchpad.height_pct, 94);
    assert_eq!(config.scratchpad.dim_alpha, 96);
    assert_eq!(config.scratchpad.autostart.len(), 1);
    assert_eq!(config.scratchpad.autostart[0], "kitty -e htop");
    assert_eq!(config.scratchpad.defines.len(), 1);
    assert_eq!(config.scratchpad.defines[0].name, "term");
    assert_eq!(config.scratchpad.defines[0].command, "kitty"); // $terminal expanded
    assert_eq!(config.scratchpad.defines[0].app_id.as_deref(), Some("kitty"));

    // Rules
    assert_eq!(config.rules.len(), 3);
    match &config.rules[0] {
        Rule::Float { app_id } => assert_eq!(app_id, "pavucontrol"),
        _ => panic!("expected float rule"),
    }
    match &config.rules[2] {
        Rule::Workspace { workspace, app_id, monitor } => {
            assert_eq!(*workspace, 7); // 8 -> 0-indexed 7
            assert_eq!(app_id, "thunderbird");
            assert!(monitor.is_none());
        }
        _ => panic!("expected workspace rule"),
    }

    // Autostart
    assert_eq!(config.autostart.len(), 2);
    assert_eq!(config.autostart[0].command, "dunst");

    // Commands — variables should be expanded
    assert_eq!(config.commands.get("terminal").unwrap(), "kitty");
    assert_eq!(config.commands.get("launcher").unwrap(), "rofi -show drun");
    assert_eq!(config.commands.get("browser").unwrap(), "floorp");

    // Keybinds
    // 11 explicit + 9 view_ws + 9 send_ws + 1 XF86 = 30
    assert_eq!(config.keybinds.len(), 30);

    // Check range expansion
    let view_ws: Vec<_> = config.keybinds.iter()
        .filter(|kb| matches!(kb.action, BindAction::ViewWs(_)))
        .collect();
    assert_eq!(view_ws.len(), 9);

    let send_ws: Vec<_> = config.keybinds.iter()
        .filter(|kb| matches!(kb.action, BindAction::SendWs(_)))
        .collect();
    assert_eq!(send_ws.len(), 9);

    // Check XF86 key (no modifier)
    let xf86 = config.keybinds.iter()
        .find(|kb| matches!(&kb.action, BindAction::Spawn(n) if n == "vol-up"))
        .expect("XF86AudioRaiseVolume bind not found");
    assert_eq!(xf86.mods, ModMask::NONE);
}

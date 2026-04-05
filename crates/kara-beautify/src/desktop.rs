use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::Result;
use kara_theme::ResolvedTheme;

pub fn merged_session_env() -> HashMap<String, String> {
    let mut map: HashMap<String, String> = env::vars().collect();

    if let Ok(out) = Command::new("tmux")
        .arg("show-environment")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if line.starts_with('-') || !line.contains('=') {
                    continue;
                }
                let mut parts = line.splitn(2, '=');
                let key = parts.next().unwrap_or_default();
                let value = parts.next().unwrap_or_default();

                if matches!(
                    key,
                    "DBUS_SESSION_BUS_ADDRESS"
                        | "DISPLAY"
                        | "XAUTHORITY"
                        | "XDG_CURRENT_DESKTOP"
                        | "XDG_RUNTIME_DIR"
                        | "GTK_USE_PORTAL"
                ) {
                    map.insert(key.to_string(), value.to_string());
                }
            }
        }
    }

    map
}

pub fn sync_desktop_appearance(theme: &ResolvedTheme) -> Result<()> {
    let env_map = merged_session_env();

    let cursor_size_str = theme.cursor.size.to_string();

    let commands = [
        vec![
            "gsettings",
            "set",
            "org.gnome.desktop.interface",
            "color-scheme",
            theme.gsettings_color_scheme(),
        ],
        vec![
            "gsettings",
            "set",
            "org.gnome.desktop.interface",
            "gtk-theme",
            theme.gtk_theme_name(),
        ],
        vec![
            "gsettings",
            "set",
            "org.gnome.desktop.interface",
            "icon-theme",
            theme.gtk_icon_theme_name(),
        ],
        vec![
            "gsettings",
            "set",
            "org.gnome.desktop.interface",
            "cursor-theme",
            &theme.cursor.theme,
        ],
        vec![
            "gsettings",
            "set",
            "org.gnome.desktop.interface",
            "cursor-size",
            &cursor_size_str,
        ],
    ];

    for cmd in commands {
        let mut c = Command::new(cmd[0]);
        c.args(&cmd[1..]);
        c.envs(&env_map);
        c.stdin(Stdio::null());
        c.stdout(Stdio::null());
        c.stderr(Stdio::null());

        let _ = c.status();
    }

    write_xcursor_default(&theme.cursor.theme);
    reload_xcursor(&theme.cursor.theme, theme.cursor.size, &env_map);

    Ok(())
}

fn write_xcursor_default(cursor_theme: &str) {
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let dir = PathBuf::from(&home).join(".icons").join("default");
    let _ = fs::create_dir_all(&dir);
    let content = format!(
        "[Icon Theme]\nName=Default\nComment=Default Cursor Theme\nInherits={}\n",
        cursor_theme
    );
    let _ = fs::write(dir.join("index.theme"), content);
}

fn reload_xcursor(cursor_theme: &str, cursor_size: u16, env_map: &HashMap<String, String>) {
    // Set env for the xsetroot child so it picks up the new cursor
    let mut c = Command::new("xsetroot");
    c.arg("-cursor_name").arg("left_ptr");
    c.env("XCURSOR_THEME", cursor_theme);
    c.env("XCURSOR_SIZE", cursor_size.to_string());
    c.envs(env_map);
    c.stdin(Stdio::null());
    c.stdout(Stdio::null());
    c.stderr(Stdio::null());
    let _ = c.status();
}

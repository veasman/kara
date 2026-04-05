use std::env;
use std::path::Path;
use std::process::Command;

use crate::state::paths::KaraPaths;

#[derive(Debug, Clone, Copy)]
pub enum DoctorStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub status: DoctorStatus,
    pub detail: String,
}

pub fn run_doctor_checks(paths: &KaraPaths) -> Vec<DoctorCheck> {
    vec![
        check_env_var("DISPLAY"),
        check_env_var("DBUS_SESSION_BUS_ADDRESS"),
        check_env_var("XDG_CURRENT_DESKTOP"),
        check_runtime_dirs(paths),
        check_command("gsettings"),
        check_command("gdbus"),
        check_command("xsetroot"),
        check_portal(),
        check_gsettings_value(),
        check_cursor_themes(),
    ]
}

fn check_env_var(key: &'static str) -> DoctorCheck {
    match env::var(key) {
        Ok(v) if !v.trim().is_empty() => DoctorCheck {
            name: key,
            status: DoctorStatus::Pass,
            detail: v,
        },
        _ => DoctorCheck {
            name: key,
            status: DoctorStatus::Warn,
            detail: "not set".to_string(),
        },
    }
}

fn check_runtime_dirs(paths: &KaraPaths) -> DoctorCheck {
    match paths.ensure_runtime_dirs() {
        Ok(_) => DoctorCheck {
            name: "runtime_dirs",
            status: DoctorStatus::Pass,
            detail: paths.generated_dir().display().to_string(),
        },
        Err(e) => DoctorCheck {
            name: "runtime_dirs",
            status: DoctorStatus::Fail,
            detail: e.to_string(),
        },
    }
}

fn check_command(name: &'static str) -> DoctorCheck {
    match Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v {name} >/dev/null 2>&1"))
        .status()
    {
        Ok(status) if status.success() => DoctorCheck {
            name,
            status: DoctorStatus::Pass,
            detail: "found".to_string(),
        },
        Ok(_) | Err(_) => DoctorCheck {
            name,
            status: DoctorStatus::Warn,
            detail: "not found".to_string(),
        },
    }
}

fn check_portal() -> DoctorCheck {
    let output = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.portal.Desktop",
            "--object-path",
            "/org/freedesktop/portal/desktop",
            "--method",
            "org.freedesktop.portal.Settings.Read",
            "org.freedesktop.appearance",
            "color-scheme",
        ])
        .output();

    match output {
        Ok(out) if out.status.success() => DoctorCheck {
            name: "portal",
            status: DoctorStatus::Pass,
            detail: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        },
        Ok(out) => DoctorCheck {
            name: "portal",
            status: DoctorStatus::Warn,
            detail: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        },
        Err(e) => DoctorCheck {
            name: "portal",
            status: DoctorStatus::Warn,
            detail: e.to_string(),
        },
    }
}

fn check_gsettings_value() -> DoctorCheck {
    let output = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output();

    match output {
        Ok(out) if out.status.success() => DoctorCheck {
            name: "gsettings_color_scheme",
            status: DoctorStatus::Pass,
            detail: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        },
        Ok(out) => DoctorCheck {
            name: "gsettings_color_scheme",
            status: DoctorStatus::Warn,
            detail: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        },
        Err(e) => DoctorCheck {
            name: "gsettings_color_scheme",
            status: DoctorStatus::Warn,
            detail: e.to_string(),
        },
    }
}

fn check_cursor_themes() -> DoctorCheck {
    let expected = ["Bibata-Modern-Classic", "Bibata-Modern-Ice", "Bibata-Modern-Amber"];
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());

    let search_dirs = [
        format!("{home}/.local/share/icons"),
        format!("{home}/.icons"),
        "/usr/share/icons".to_string(),
    ];

    let mut found = vec![];
    let mut missing = vec![];

    for name in &expected {
        let exists = search_dirs
            .iter()
            .any(|dir| Path::new(dir).join(name).join("cursors").is_dir());
        if exists {
            found.push(*name);
        } else {
            missing.push(*name);
        }
    }

    if missing.is_empty() {
        DoctorCheck {
            name: "cursor_themes",
            status: DoctorStatus::Pass,
            detail: format!("found: {}", found.join(", ")),
        }
    } else {
        DoctorCheck {
            name: "cursor_themes",
            status: DoctorStatus::Warn,
            detail: format!(
                "missing: {} (install bibata-cursor-theme)",
                missing.join(", ")
            ),
        }
    }
}

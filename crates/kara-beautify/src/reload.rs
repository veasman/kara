use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, Default)]
pub struct ReloadPlan {
    pub kara_gate: bool,
    pub kitty: bool,
    pub foot: bool,
    /// Whether the theme's active mode is dark. reload_foot needs to
    /// know so it can round-trip through the correct mode signals —
    /// SIGUSR2 → SIGUSR1 for dark themes, SIGUSR1 → SIGUSR2 for
    /// light. Ignored when `foot` is false.
    pub foot_dark: bool,
    pub tmux: bool,
    pub nvim: bool,
}

impl ReloadPlan {
    pub fn any(self) -> bool {
        self.kara_gate || self.kitty || self.foot || self.tmux || self.nvim
    }
}

fn run_shell(command: &str) {
    let _ = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub fn reload_kara_gate() {
    // Try IPC first, fall back to signal
    if let Ok(mut client) = kara_ipc::client::IpcClient::connect() {
        let req = kara_ipc::Request::ThemeChanged {
            theme_name: String::new(),
        };
        let _ = client.request(&req);
    } else {
        run_shell(r#"pidof kara-gate >/dev/null 2>&1 && kill -HUP "$(pidof kara-gate)" || true"#);
    }
}

pub fn reload_kitty() {
    run_shell(r#"pidof kitty >/dev/null 2>&1 && pkill -USR1 -x kitty || true"#);
}

pub fn reload_foot(dark_mode: bool) {
    // foot's SIGUSR1/SIGUSR2 are "switch to dark theme" and "switch
    // to light theme" signals respectively — NOT a config reload.
    // But a round-trip (switch to the opposite mode then back)
    // forces foot to re-read the active `[colors-<mode>]` section
    // from disk, which is effectively our live-reload mechanism.
    //
    // Sequence:
    //   dark theme  → USR2 (light), then USR1 (back to dark)
    //   light theme → USR1 (dark),  then USR2 (back to light)
    //
    // The intermediate signal briefly switches foot to the "wrong"
    // mode — if the corresponding [colors-<other>] section isn't
    // in the config, foot falls back to internal defaults for
    // ~50ms before we swing back. Small flicker, hard to notice
    // in practice, and it's the only way to live-reload colors
    // on foot 1.26 without killing and respawning the server.
    let (first, second) = if dark_mode {
        ("USR2", "USR1")
    } else {
        ("USR1", "USR2")
    };
    let command = format!(
        r#"pidof foot >/dev/null 2>&1 && pkill -{first} -x foot && sleep 0.05 && pkill -{second} -x foot || true"#
    );
    run_shell(&command);
}

pub fn reload_tmux(theme_path: &Path) {
    let command = format!(
        "tmux source-file {} >/dev/null 2>&1 || true",
        theme_path.display()
    );
    run_shell(&command);
}

pub fn reload_nvim_sessions() {
    run_shell(
        r#"for sock in /tmp/nvim-kara-*.sock; do [ -S "$sock" ] || continue; nvim --server "$sock" --remote-send "<Esc>:silent! KaraReloadTheme<CR>" >/dev/null 2>&1 || true; done"#,
    );
}

pub fn apply_runtime_reloads(plan: ReloadPlan, tmux_theme_path: &Path) {
    if !plan.any() {
        return;
    }

    if plan.kara_gate {
        reload_kara_gate();
    }
    if plan.kitty {
        reload_kitty();
    }
    if plan.foot {
        reload_foot(plan.foot_dark);
    }
    if plan.tmux {
        reload_tmux(tmux_theme_path);
    }
    if plan.nvim {
        reload_nvim_sessions();
    }
}

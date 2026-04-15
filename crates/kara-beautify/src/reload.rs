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

pub fn reload_foot(_dark_mode: bool) {
    // No-op by design. foot 1.26 does NOT have a config reload
    // mechanism — SIGUSR1/SIGUSR2 are theme-toggle signals that
    // switch between [colors] and [colors2] sections that foot
    // CACHED at server startup. They do not re-read foot.ini
    // from disk. Confirmed by reading foot's CHANGELOG.md and
    // empirically testing a SIGUSR2 → SIGUSR1 round-trip (the
    // intermediate mode briefly shows internal defaults but the
    // final state is foot's cached startup colors, not the
    // updated file).
    //
    // kara-beautify still patches foot.ini on every apply so
    // the NEXT foot --server session picks up the new theme,
    // but the running server stays on whatever colors it loaded
    // at its startup until the user restarts `foot --server`.
    //
    // A future [consumers.foot.reload_strategy] = "restart"
    // opt-in could kill and respawn the foot server on every
    // apply for real live reload, at the cost of disconnecting
    // every attached footclient. Deferred until session
    // persistence (item J in the plan) makes the "reattach my
    // tmux sessions" part of the cost automatic.
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

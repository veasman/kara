use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, Default)]
pub struct ReloadPlan {
    pub kara_gate: bool,
    pub kitty: bool,
    pub tmux: bool,
    pub nvim: bool,
}

impl ReloadPlan {
    pub fn any(self) -> bool {
        self.kara_gate || self.kitty || self.tmux || self.nvim
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
    if plan.tmux {
        reload_tmux(tmux_theme_path);
    }
    if plan.nvim {
        reload_nvim_sessions();
    }
}

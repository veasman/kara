use std::process::Command;

use crate::state::Vwm;
use crate::workspace::MFACT_STEP;

#[derive(Debug, Clone)]
pub enum Action {
    None,
    Spawn(String),
    KillClient,
    FocusNext,
    FocusPrev,
    ZoomMaster,
    ToggleMonocle,
    ToggleFullscreen,
    DecreaseMfact,
    IncreaseMfact,
    ViewWs(usize),
    SendWs(usize),
    Quit,
}

impl Vwm {
    pub fn dispatch_action(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Spawn(cmd) => self.spawn(&cmd),
            Action::KillClient => self.kill_focused(),
            Action::FocusNext => self.do_focus_next(),
            Action::FocusPrev => self.do_focus_prev(),
            Action::ZoomMaster => self.do_zoom_master(),
            Action::ToggleMonocle => self.do_toggle_monocle(),
            Action::ToggleFullscreen => self.do_toggle_fullscreen(),
            Action::DecreaseMfact => self.do_adjust_mfact(-MFACT_STEP),
            Action::IncreaseMfact => self.do_adjust_mfact(MFACT_STEP),
            Action::ViewWs(idx) => self.do_view_ws(idx),
            Action::SendWs(idx) => self.do_send_ws(idx),
            Action::Quit => {
                self.running = false;
                self.loop_signal.stop();
            }
        }
    }

    fn spawn(&self, cmd: &str) {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.is_empty() {
            return;
        }

        tracing::info!("spawning: {}", cmd);

        if let Err(e) = Command::new(parts[0])
            .args(&parts[1..])
            .spawn()
        {
            tracing::error!("failed to spawn '{}': {}", cmd, e);
        }
    }

    fn kill_focused(&mut self) {
        let ws = &self.workspaces[self.current_ws];
        if let Some(window) = ws.focused() {
            let window = window.clone();
            window.toplevel().unwrap().send_close();
        }
    }

    fn do_focus_next(&mut self) {
        self.workspaces[self.current_ws].focus_next();
        self.apply_focus();
        self.apply_layout();
    }

    fn do_focus_prev(&mut self) {
        self.workspaces[self.current_ws].focus_prev();
        self.apply_focus();
        self.apply_layout();
    }

    fn do_zoom_master(&mut self) {
        self.workspaces[self.current_ws].zoom_master();
        self.apply_layout();
        self.apply_focus();
    }

    fn do_toggle_monocle(&mut self) {
        self.workspaces[self.current_ws].toggle_layout();
        self.apply_layout();
    }

    fn do_toggle_fullscreen(&mut self) {
        // TODO: implement fullscreen toggle
        tracing::debug!("fullscreen toggle not yet implemented");
    }

    fn do_adjust_mfact(&mut self, delta: f32) {
        self.workspaces[self.current_ws].adjust_mfact(delta);
        self.apply_layout();
    }

    fn do_view_ws(&mut self, idx: usize) {
        if idx >= self.workspaces.len() || idx == self.current_ws {
            return;
        }

        tracing::debug!("switching to workspace {}", idx + 1);

        // Hide current workspace windows
        for window in &self.workspaces[self.current_ws].clients {
            self.space.unmap_elem(window);
        }

        self.previous_ws = self.current_ws;
        self.current_ws = idx;

        // Show new workspace windows via layout
        self.apply_layout();
        self.apply_focus();
    }

    fn do_send_ws(&mut self, idx: usize) {
        if idx >= self.workspaces.len() || idx == self.current_ws {
            return;
        }

        let ws = &mut self.workspaces[self.current_ws];
        let window = match ws.focused() {
            Some(w) => w.clone(),
            None => return,
        };

        tracing::debug!("sending window to workspace {}", idx + 1);

        // Remove from current workspace
        ws.remove_client(&window);

        // Unmap from space
        self.space.unmap_elem(&window);

        // Add to target workspace
        self.workspaces[idx].add_client(window);

        // Re-layout current workspace
        self.apply_layout();
        self.apply_focus();
    }
}

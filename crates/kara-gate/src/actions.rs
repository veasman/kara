use std::process::Command;

use crate::state::Gate;
use crate::workspace::MFACT_STEP;

#[derive(Debug, Clone)]
pub enum Action {
    /// Spawn a named command (looked up in config commands map)
    Spawn(String),
    /// Spawn a raw command string (no lookup)
    SpawnRaw(String),
    KillClient,
    FocusNext,
    FocusPrev,
    ZoomMaster,
    ToggleMonocle,
    ToggleFullscreen,
    ToggleFloat,
    ToggleScratchpad(Option<String>),
    DecreaseMfact,
    IncreaseMfact,
    FocusMonitorNext,
    FocusMonitorPrev,
    SendMonitorNext,
    SendMonitorPrev,
    ToggleSync,
    ViewWs(usize),
    SendWs(usize),
    Reload,
    Quit,
}

impl Gate {
    pub fn dispatch_action(&mut self, action: Action) {
        match action {
            Action::Spawn(name) => self.spawn_named(&name),
            Action::SpawnRaw(cmd) => self.spawn_raw(&cmd),
            Action::KillClient => self.kill_focused(),
            Action::FocusNext => self.do_focus_next(),
            Action::FocusPrev => self.do_focus_prev(),
            Action::ZoomMaster => self.do_zoom_master(),
            Action::ToggleMonocle => self.do_toggle_monocle(),
            Action::ToggleFullscreen => self.do_toggle_fullscreen(),
            Action::ToggleFloat => self.do_toggle_float(),
            Action::ToggleScratchpad(name) => self.do_toggle_scratchpad(name),
            Action::DecreaseMfact => self.do_adjust_mfact(-MFACT_STEP),
            Action::IncreaseMfact => self.do_adjust_mfact(MFACT_STEP),
            Action::FocusMonitorNext => self.do_focus_monitor(1),
            Action::FocusMonitorPrev => self.do_focus_monitor(-1),
            Action::SendMonitorNext => self.do_send_monitor(1),
            Action::SendMonitorPrev => self.do_send_monitor(-1),
            Action::ToggleSync => self.do_toggle_sync(),
            Action::ViewWs(idx) => self.do_view_ws(idx),
            Action::SendWs(idx) => self.do_send_ws(idx),
            Action::Reload => self.reload_config(),
            Action::Quit => {
                self.running = false;
                self.loop_signal.stop();
            }
        }
    }

    /// Spawn a named command from the config commands map.
    fn spawn_named(&self, name: &str) {
        match self.config.commands.get(name) {
            Some(cmd) => self.spawn_raw(cmd),
            None => tracing::error!("unknown command name '{name}'"),
        }
    }

    /// Spawn a raw command string via sh -c (supports pipes, redirects, etc.)
    fn spawn_raw(&self, cmd: &str) {
        if cmd.is_empty() {
            return;
        }

        tracing::info!("spawning: {}", cmd);

        if let Err(e) = Command::new("sh")
            .args(["-c", cmd])
            .spawn()
        {
            tracing::error!("failed to spawn '{}': {}", cmd, e);
        }
    }

    /// Re-layout after a scratchpad or regular workspace mutation.
    fn relayout_active(&mut self) {
        if let Some(sp_idx) = self.focused_scratchpad {
            self.apply_scratchpad_layout(sp_idx);
        } else {
            self.apply_layout();
        }
        self.apply_focus();
    }

    fn kill_focused(&mut self) {
        if let Some(sp_idx) = self.focused_scratchpad {
            let ws = &self.scratchpads[sp_idx].workspace;
            if let Some(window) = ws.focused() {
                window.clone().toplevel().unwrap().send_close();
            }
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            let ws = &self.workspaces[ws_idx];
            if let Some(window) = ws.focused() {
                window.clone().toplevel().unwrap().send_close();
            }
        }
    }

    fn do_focus_next(&mut self) {
        if let Some(sp_idx) = self.focused_scratchpad {
            self.scratchpads[sp_idx].workspace.focus_next();
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            self.workspaces[ws_idx].focus_next();
        }
        self.relayout_active();
    }

    fn do_focus_prev(&mut self) {
        if let Some(sp_idx) = self.focused_scratchpad {
            self.scratchpads[sp_idx].workspace.focus_prev();
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            self.workspaces[ws_idx].focus_prev();
        }
        self.relayout_active();
    }

    fn do_zoom_master(&mut self) {
        if let Some(sp_idx) = self.focused_scratchpad {
            self.scratchpads[sp_idx].workspace.zoom_master();
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            self.workspaces[ws_idx].zoom_master();
        }
        self.relayout_active();
    }

    fn do_toggle_monocle(&mut self) {
        if let Some(sp_idx) = self.focused_scratchpad {
            self.scratchpads[sp_idx].workspace.toggle_layout();
            self.apply_scratchpad_layout(sp_idx);
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            self.workspaces[ws_idx].toggle_layout();
            self.apply_layout();
        }
    }

    fn do_toggle_fullscreen(&mut self) {
        // Block fullscreen for scratchpad windows
        if self.focused_scratchpad.is_some() {
            tracing::debug!("fullscreen not available in scratchpad");
            return;
        }

        let out_idx = self.focused_output;
        let has_fs = self.outputs.get(out_idx)
            .map(|o| o.fullscreen_window.is_some())
            .unwrap_or(false);

        if has_fs {
            if let Some(out) = self.outputs.get_mut(out_idx) {
                out.fullscreen_window = None;
            }
            self.apply_layout();
            self.apply_focus();
            tracing::debug!("exited fullscreen");
        } else {
            let ws_idx = self.effective_ws(out_idx);
            let ws = &self.workspaces[ws_idx];
            if let Some(window) = ws.focused() {
                let window = window.clone();
                let (w, h) = self.output_size();
                let loc = self.outputs[out_idx].location;

                if let Some(out) = self.outputs.get_mut(out_idx) {
                    out.fullscreen_window = Some(window.clone());
                }

                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.size = Some((w, h).into());
                    });
                    toplevel.send_configure();
                }
                self.space.map_element(window, loc, false);
                tracing::debug!("entered fullscreen");
            }
        }
    }

    fn do_toggle_float(&mut self) {
        if let Some(sp_idx) = self.focused_scratchpad {
            self.scratchpads[sp_idx].workspace.toggle_focused_floating();
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            self.workspaces[ws_idx].toggle_focused_floating();
        }
        self.relayout_active();
    }

    fn do_toggle_scratchpad(&mut self, name: Option<String>) {
        // Find scratchpad by name (default: first)
        let sp_name = name.as_deref().unwrap_or("main");
        let sp_idx = match self.scratchpads.iter().position(|sp| {
            self.config.scratchpads.get(sp.config_idx)
                .map(|sc| sc.name == sp_name)
                .unwrap_or(false)
        }) {
            Some(idx) => idx,
            None => {
                tracing::warn!("no scratchpad named '{sp_name}'");
                return;
            }
        };

        if self.scratchpads[sp_idx].visible {
            // Hide this scratchpad
            let preset = self.config.animations.preset;
            let duration = self.config.animations.duration_ms;
            let windows: Vec<_> = self.scratchpads[sp_idx].workspace.clients.clone();

            if preset != kara_config::AnimationPreset::Instant && duration > 0 {
                let wa = self.workarea();
                for window in &windows {
                    if let Some(loc) = self.space.element_location(window) {
                        let geom = window.geometry();
                        self.animations.animate_out(
                            window.clone(), preset, duration,
                            loc.x, loc.y, geom.size.w, geom.size.h,
                            wa.loc.x, wa.loc.y, wa.size.w, wa.size.h,
                            crate::animation::SlideDirection::Auto,
                        );
                    }
                }
            } else {
                for window in &windows {
                    self.space.unmap_elem(window);
                }
            }

            self.scratchpads[sp_idx].visible = false;
            if self.focused_scratchpad == Some(sp_idx) {
                self.focused_scratchpad = None;
            }
            self.apply_layout();
            self.apply_focus();
            tracing::debug!("scratchpad '{sp_name}' hidden");
            return;
        }

        // Autostart on first toggle
        if !self.scratchpads[sp_idx].started {
            self.scratchpads[sp_idx].started = true;
            if let Some(cmd) = self.config.scratchpads.get(sp_idx)
                .and_then(|sc| sc.autostart.clone())
            {
                // Mark pending capture so the spawned window goes to this scratchpad
                self.scratchpads[sp_idx].pending_capture = true;
                self.spawn_raw(&cmd);
            }
        }

        // Show scratchpad
        self.scratchpads[sp_idx].visible = true;
        self.scratchpads[sp_idx].output_idx = self.focused_output;
        self.focused_scratchpad = Some(sp_idx);

        // Layout regular workspace first (populates border_rects), then scratchpad on top
        self.apply_layout();
        self.apply_scratchpad_layout(sp_idx);

        // Animate windows in
        let preset = self.config.animations.preset;
        if preset != kara_config::AnimationPreset::Instant {
            let wa = self.workarea();
            let windows: Vec<_> = self.scratchpads[sp_idx].workspace.clients.clone();
            for window in &windows {
                if let Some(loc) = self.space.element_location(window) {
                    let geom = window.geometry();
                    self.animations.animate_in(
                        window.clone(), preset, self.config.animations.duration_ms,
                        loc.x, loc.y, geom.size.w, geom.size.h,
                        wa.loc.x, wa.loc.y, wa.size.w, wa.size.h,
                        crate::animation::SlideDirection::Auto,
                    );
                }
            }
        }

        self.apply_focus();
        tracing::debug!(
            "scratchpad '{sp_name}' shown ({} windows)",
            self.scratchpads[sp_idx].workspace.clients.len(),
        );
    }

    fn do_adjust_mfact(&mut self, delta: f32) {
        if let Some(sp_idx) = self.focused_scratchpad {
            self.scratchpads[sp_idx].workspace.adjust_mfact(delta);
            self.apply_scratchpad_layout(sp_idx);
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            self.workspaces[ws_idx].adjust_mfact(delta);
            self.apply_layout();
        }
    }

    fn do_view_ws(&mut self, idx: usize) {
        if idx >= self.workspaces.len() {
            return;
        }

        let current = self.effective_ws(self.focused_output);
        if idx == current {
            return;
        }

        tracing::debug!("switching to workspace {}", idx + 1);

        if self.config.general.sync_workspaces {
            // Sync mode: all outputs switch together
            self.previous_ws = self.current_ws;
            self.current_ws = idx;
        } else {
            // Independent mode: only focused output switches
            // If target ws is visible on another output, swap
            let mut swap_output = None;
            for (i, out) in self.outputs.iter().enumerate() {
                if i != self.focused_output && out.current_ws == idx {
                    swap_output = Some(i);
                    break;
                }
            }

            let old_ws = self.outputs[self.focused_output].current_ws;
            self.previous_ws = old_ws;

            if let Some(other) = swap_output {
                // Swap: other output gets our old workspace
                self.outputs[other].current_ws = old_ws;
            }

            self.outputs[self.focused_output].current_ws = idx;
            self.current_ws = idx; // keep in sync for focused output
        }

        self.apply_layout();
        self.apply_focus();

        // Animate incoming windows
        let preset = self.config.animations.preset;
        if preset != kara_config::AnimationPreset::Instant {
            let direction = if idx > current {
                crate::animation::SlideDirection::Right
            } else {
                crate::animation::SlideDirection::Left
            };
            let ws_idx = self.effective_ws(self.focused_output);
            let wa = self.workarea();
            for window in self.workspaces[ws_idx].clients.clone() {
                if let Some(loc) = self.space.element_location(&window) {
                    let geom = window.geometry();
                    self.animations.animate_in(
                        window, preset, self.config.animations.duration_ms,
                        loc.x, loc.y, geom.size.w, geom.size.h,
                        wa.loc.x, wa.loc.y, wa.size.w, wa.size.h,
                        direction,
                    );
                }
            }
        }
    }

    fn do_send_ws(&mut self, idx: usize) {
        let current = self.effective_ws(self.focused_output);
        if idx >= self.workspaces.len() || idx == current {
            return;
        }

        let ws = &mut self.workspaces[current];
        let window = match ws.focused() {
            Some(w) => w.clone(),
            None => return,
        };

        tracing::debug!("sending window to workspace {}", idx + 1);

        let preset = self.config.animations.preset;
        if preset != kara_config::AnimationPreset::Instant && self.config.animations.duration_ms > 0 {
            // Animate out, then defer the actual transfer
            if let Some(loc) = self.space.element_location(&window) {
                let geom = window.geometry();
                let wa = self.workarea();
                let direction = if idx > current {
                    crate::animation::SlideDirection::Right
                } else {
                    crate::animation::SlideDirection::Left
                };
                self.animations.animate_out(
                    window.clone(), preset, self.config.animations.duration_ms,
                    loc.x, loc.y, geom.size.w, geom.size.h,
                    wa.loc.x, wa.loc.y, wa.size.w, wa.size.h,
                    direction,
                );
                self.pending_sends.push((window, idx));
                return;
            }
        }

        // Instant path: remove, unmap, transfer immediately
        ws.remove_client(&window);
        self.space.unmap_elem(&window);
        self.workspaces[idx].add_client(window);
        self.apply_layout();
        self.apply_focus();
    }

    // ── Multi-monitor actions ────────────────────────────────────────

    fn do_focus_monitor(&mut self, direction: i32) {
        if self.outputs.len() <= 1 {
            return;
        }
        let count = self.outputs.len() as i32;
        let new_idx = ((self.focused_output as i32 + direction).rem_euclid(count)) as usize;
        self.focused_output = new_idx;

        // Warp pointer to center of new output
        let out = &self.outputs[new_idx];
        self.pointer_location = (
            out.location.x as f64 + out.size.0 as f64 / 2.0,
            out.location.y as f64 + out.size.1 as f64 / 2.0,
        ).into();

        self.apply_focus();
        tracing::debug!("focused monitor {new_idx}");
    }

    fn do_send_monitor(&mut self, direction: i32) {
        if self.outputs.len() <= 1 {
            return;
        }
        let count = self.outputs.len() as i32;
        let target = ((self.focused_output as i32 + direction).rem_euclid(count)) as usize;

        let src_ws = self.effective_ws(self.focused_output);
        let dst_ws = self.effective_ws(target);

        let window = match self.workspaces[src_ws].focused() {
            Some(w) => w.clone(),
            None => return,
        };

        tracing::debug!("sending window to monitor {target}");

        self.workspaces[src_ws].remove_client(&window);
        self.space.unmap_elem(&window);
        self.workspaces[dst_ws].add_client(window);

        self.apply_layout();
        self.apply_focus();
    }

    fn do_toggle_sync(&mut self) {
        let was_sync = self.config.general.sync_workspaces;
        self.config.general.sync_workspaces = !was_sync;

        if self.config.general.sync_workspaces {
            // Entering sync mode: all outputs show focused output's workspace
            let ws = self.outputs.get(self.focused_output)
                .map(|o| o.current_ws)
                .unwrap_or(0);
            self.current_ws = ws;
        } else {
            // Entering independent mode: spread outputs across workspaces
            for (i, out) in self.outputs.iter_mut().enumerate() {
                if i == self.focused_output {
                    out.current_ws = self.current_ws;
                } else {
                    // Assign a different workspace if possible
                    let candidate = (self.current_ws + i) % self.workspaces.len();
                    out.current_ws = candidate;
                }
            }
        }

        tracing::info!(
            "workspace sync: {}",
            if self.config.general.sync_workspaces { "on" } else { "off" }
        );

        self.apply_layout();
        self.apply_focus();
    }

    // ── Environment ────────────────────────────────────────────────

    /// Apply environment directives from config.
    pub fn apply_environment(&self) {
        for directive in &self.config.environment {
            match directive {
                kara_config::EnvDirective::Set { key, value } => {
                    tracing::debug!("env set: {key}={value}");
                    unsafe { std::env::set_var(key, value) };
                }
                kara_config::EnvDirective::Source { path } => {
                    let expanded = if path.starts_with('~') {
                        if let Some(home) = dirs::home_dir() {
                            home.join(&path[2..]).to_string_lossy().to_string()
                        } else {
                            path.clone()
                        }
                    } else {
                        path.clone()
                    };

                    tracing::debug!("env source: {expanded}");
                    match std::fs::read_to_string(&expanded) {
                        Ok(contents) => {
                            for line in contents.lines() {
                                let trimmed = line.trim();
                                if trimmed.is_empty() || trimmed.starts_with('#') {
                                    continue;
                                }
                                if let Some((key, value)) = trimmed.split_once('=') {
                                    let key = key.trim();
                                    let value = value.trim().trim_matches('"').trim_matches('\'');
                                    if !key.is_empty() {
                                        unsafe { std::env::set_var(key, value) };
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("failed to source '{expanded}': {e}");
                        }
                    }
                }
            }
        }
    }

    // ── Cursor theme ───────────────────────────────────────────────

    /// Set cursor theme environment variables from config.
    pub fn apply_cursor_theme(&self) {
        if let Some(ref theme) = self.config.general.cursor_theme {
            tracing::info!("cursor theme: {theme}, size: {}", self.config.general.cursor_size);
            unsafe {
                std::env::set_var("XCURSOR_THEME", theme);
                std::env::set_var("XCURSOR_SIZE", self.config.general.cursor_size.to_string());
            }
        }
    }

    /// Load cursor theme images into cache for software rendering.
    pub fn load_cursor_theme(&mut self) {
        let theme_name = self.config.general.cursor_theme
            .as_deref()
            .unwrap_or("default");
        let size = self.config.general.cursor_size as u32;

        match crate::cursor::load_xcursor(theme_name, "default", size) {
            Some(cache) => {
                tracing::info!(
                    "loaded cursor: {}x{} from theme '{}'",
                    cache.width, cache.height, theme_name,
                );
                self.cursor_cache = Some(cache);
            }
            None => {
                tracing::warn!("failed to load cursor theme '{theme_name}', cursor may not render");
                self.cursor_cache = None;
            }
        }
    }

    // ── Autostart ──────────────────────────────────────────────────

    /// Run autostart commands from config (only once, skipped on reload).
    pub fn run_autostart(&mut self) {
        if self.autostart_done {
            return;
        }
        self.autostart_done = true;

        for entry in &self.config.autostart {
            tracing::info!("autostart: {}", entry.command);
            if let Err(e) = Command::new("sh")
                .args(["-c", &entry.command])
                .spawn()
            {
                tracing::error!("autostart failed '{}': {}", entry.command, e);
            }
        }
    }

    // ── Window rules helpers ───────────────────────────────────────

    /// Check window rules for a given app_id. Returns (should_float, target_workspace).
    pub fn check_rules(&self, app_id: &str) -> (bool, Option<usize>) {
        let mut should_float = false;
        let mut target_ws = None;

        for rule in &self.config.rules {
            match rule {
                kara_config::Rule::Float { app_id: rule_id } => {
                    if rule_id == app_id {
                        should_float = true;
                    }
                }
                kara_config::Rule::Workspace {
                    workspace,
                    app_id: rule_id,
                    ..
                } => {
                    if rule_id == app_id {
                        target_ws = Some(*workspace);
                    }
                }
            }
        }

        (should_float, target_ws)
    }

    /// Check if a window belongs to a scratchpad capture and return the scratchpad index.
    pub fn check_scratchpad_capture(&self, app_id: &str) -> Option<usize> {
        for (i, sc) in self.config.scratchpads.iter().enumerate() {
            if sc.captures.iter().any(|pattern| pattern == app_id) {
                return Some(i);
            }
        }
        None
    }
}

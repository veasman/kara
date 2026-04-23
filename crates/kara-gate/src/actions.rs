use std::process::Command;

use crate::state::Gate;
use crate::workspace::{MFACT_STEP, WORKSPACE_COUNT};

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
    ShowKeybinds,
    Reload,
    Quit,
    /// Built-in session lock. Blanks the render path BEFORE spawning
    /// kara-veil so the desktop is never visible during the
    /// client's startup (Wayland connect + IPC + lock-surface setup).
    /// The protocol-level lock takes over once kara-veil's lock
    /// request arrives; if kara-veil fails to show up within a few
    /// seconds, the pre-lock clears so the user isn't stranded.
    Lock,
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
            Action::ShowKeybinds => {
                self.keybind_overlay_visible = !self.keybind_overlay_visible;
                self.layout_dirty = true;
            }
            Action::Reload => self.reload_config(),
            Action::Quit => {
                self.running = false;
                self.loop_signal.stop();
            }
            Action::Lock => self.do_lock(),
        }
    }

    /// Session lock: flip the render path to "locked" immediately so
    /// the desktop vanishes this frame, then spawn kara-veil. When
    /// kara-veil's ext-session-lock request lands, the protocol-level
    /// lock takes over and `lock_pending_since` is cleared. If kara-
    /// veil never shows up (crashed, missing binary), a timer in the
    /// main loop clears `lock_pending_since` after 5 s so the user
    /// isn't stuck staring at their wallpaper with no prompt.
    fn do_lock(&mut self) {
        if self.session_lock.is_some() {
            return; // already locked, nothing to do
        }
        self.lock_pending_since = Some(std::time::Instant::now());
        self.layout_dirty = true;
        self.bar_dirty = true;
        self.spawn_raw("kara-veil");
    }

    /// Spawn a named command from the config commands map.
    fn spawn_named(&self, name: &str) {
        match self.config.commands.get(name) {
            Some(cmd) => self.spawn_raw(cmd),
            None => tracing::error!("unknown command name '{name}'"),
        }
    }

    /// Spawn a raw command string via sh -c (supports pipes, redirects, etc.)
    pub(crate) fn spawn_raw(&self, cmd: &str) {
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
        if let Some(sp_idx) = self.active_scratchpad_for_focus() {
            self.apply_scratchpad_layout(sp_idx);
        } else {
            self.apply_layout();
        }
        self.apply_focus();
    }

    fn kill_focused(&mut self) {
        if let Some(sp_idx) = self.active_scratchpad_for_focus() {
            let ws = &self.scratchpads[sp_idx].workspace;
            if let Some(window) = ws.focused() {
                window.clone().toplevel().unwrap().send_close();
            }
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            let ws = &self.workspaces[self.focused_output][ws_idx];
            if let Some(window) = ws.focused() {
                window.clone().toplevel().unwrap().send_close();
            }
        }
    }

    fn do_focus_next(&mut self) {
        if let Some(sp_idx) = self.active_scratchpad_for_focus() {
            self.scratchpads[sp_idx].workspace.focus_next();
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            self.workspaces[self.focused_output][ws_idx].focus_next();
        }
        self.relayout_active();
    }

    fn do_focus_prev(&mut self) {
        if let Some(sp_idx) = self.active_scratchpad_for_focus() {
            self.scratchpads[sp_idx].workspace.focus_prev();
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            self.workspaces[self.focused_output][ws_idx].focus_prev();
        }
        self.relayout_active();
    }

    fn do_zoom_master(&mut self) {
        if let Some(sp_idx) = self.active_scratchpad_for_focus() {
            self.scratchpads[sp_idx].workspace.zoom_master();
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            self.workspaces[self.focused_output][ws_idx].zoom_master();
        }
        self.relayout_active();
    }

    fn do_toggle_monocle(&mut self) {
        if let Some(sp_idx) = self.active_scratchpad_for_focus() {
            self.scratchpads[sp_idx].workspace.toggle_layout();
            self.apply_scratchpad_layout(sp_idx);
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            self.workspaces[self.focused_output][ws_idx].toggle_layout();
            self.apply_layout();
        }
    }

    fn do_toggle_fullscreen(&mut self) {
        // Block fullscreen for scratchpad windows
        if self.active_scratchpad_for_focus().is_some() {
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
            let ws = &self.workspaces[self.focused_output][ws_idx];
            if let Some(window) = ws.focused() {
                let window = window.clone();
                if let Some(out) = self.outputs.get_mut(out_idx) {
                    out.fullscreen_window = Some(window.clone());
                }
                self.apply_layout();
                self.apply_focus();
                tracing::debug!("entered fullscreen");
            }
        }
    }

    fn do_toggle_float(&mut self) {
        if let Some(sp_idx) = self.active_scratchpad_for_focus() {
            self.scratchpads[sp_idx].workspace.toggle_focused_floating();
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            self.workspaces[self.focused_output][ws_idx].toggle_focused_floating();
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
            // Three cases when toggling a visible scratchpad:
            //
            // (a) It's on the focused output → hide it.
            // (b) It's on a different output → MOVE it to the focused output
            //     instead of hiding. Re-anchor the scratchpad's `output_idx`
            //     and re-layout so the windows reposition in the new monitor's
            //     workarea. The user thinks of the scratchpad as one object
            //     that follows them across monitors.
            if self.scratchpads[sp_idx].output_idx != self.focused_output {
                self.scratchpads[sp_idx].output_idx = self.focused_output;
                self.focused_scratchpad = Some(sp_idx);
                self.apply_layout();
                self.apply_scratchpad_layout(sp_idx);
                self.apply_focus();
                self.bar_dirty = true;
                tracing::debug!(
                    "scratchpad '{sp_name}' moved to {}",
                    self.outputs[self.focused_output].output.name()
                );
                return;
            }

            // (a) — hide
            let windows: Vec<_> = self.scratchpads[sp_idx].workspace.clients.clone();

            // Instant hide — windows, borders, dim all disappear in the same frame.
            // Animated hide causes visual desync (content smaller than border).
            for window in &windows {
                self.animations.cancel(window);
                self.space.unmap_elem(window);
            }
            self.scratchpad_border_rects.clear();
            self.scratchpad_border_cache.clear();
            self.scratchpad_border_offsets.clear();

            self.scratchpads[sp_idx].visible = false;
            if self.focused_scratchpad == Some(sp_idx) {
                self.focused_scratchpad = None;
            }
            self.apply_layout();
            self.apply_focus();
            tracing::debug!("scratchpad '{sp_name}' hidden");
            return;
        }

        // One-scratchpad-per-monitor: if any OTHER scratchpad is
        // currently visible on the focused output, hide it first so the
        // new one replaces it rather than stacking. This matches the
        // mental model of "the scratchpad slot" as a single overlay
        // per monitor.
        let focused_out = self.focused_output;
        let others_to_hide: Vec<usize> = self
            .scratchpads
            .iter()
            .enumerate()
            .filter(|(i, sp)| *i != sp_idx && sp.visible && sp.output_idx == focused_out)
            .map(|(i, _)| i)
            .collect();
        let had_others = !others_to_hide.is_empty();
        for other_idx in others_to_hide {
            let windows: Vec<_> = self.scratchpads[other_idx].workspace.clients.clone();
            for window in &windows {
                self.animations.cancel(window);
                self.space.unmap_elem(window);
            }
            self.scratchpads[other_idx].visible = false;
            if self.focused_scratchpad == Some(other_idx) {
                self.focused_scratchpad = None;
            }
            let other_name = self
                .config
                .scratchpads
                .get(self.scratchpads[other_idx].config_idx)
                .map(|s| s.name.as_str())
                .unwrap_or("?")
                .to_string();
            tracing::debug!(
                "scratchpad '{other_name}' auto-hidden (swapping to '{sp_name}')"
            );
        }
        if had_others {
            self.scratchpad_border_rects.clear();
            self.scratchpad_border_cache.clear();
            self.scratchpad_border_offsets.clear();
        }

        // Autostart on first toggle. A scratchpad can declare multiple
        // `autostart` entries — we seed `autostart_remaining` with the
        // full list and spawn only the first one. The rest are
        // spawned sequentially as each prior window maps (see the
        // capture path in state.rs::map_new_toplevel). Serial spawning
        // is what guarantees declaration order; spawning all N up
        // front lets process startup races scramble the order.
        if !self.scratchpads[sp_idx].started {
            self.scratchpads[sp_idx].started = true;
            let cmds: Vec<String> = self
                .config
                .scratchpads
                .get(sp_idx)
                .map(|sc| sc.autostart.clone())
                .unwrap_or_default();
            if !cmds.is_empty() {
                tracing::info!(
                    "scratchpad '{sp_name}' autostart seeded with {} cmds: {:?}",
                    cmds.len(), cmds,
                );
                self.scratchpads[sp_idx].autostart_remaining = cmds;
                let first = self.scratchpads[sp_idx].autostart_remaining[0].clone();
                self.spawn_raw(&first);
            }
        }

        // Show scratchpad
        self.scratchpads[sp_idx].visible = true;
        self.scratchpads[sp_idx].output_idx = self.focused_output;
        self.focused_scratchpad = Some(sp_idx);

        // Layout regular workspace (populates border_rects — apply_layout auto-unmaps
        // workspace windows when scratchpad is active), then layout scratchpad on top.
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
        if let Some(sp_idx) = self.active_scratchpad_for_focus() {
            self.scratchpads[sp_idx].workspace.adjust_mfact(delta);
            self.apply_scratchpad_layout(sp_idx);
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            self.workspaces[self.focused_output][ws_idx].adjust_mfact(delta);
            self.apply_layout();
        }
    }

    fn do_view_ws(&mut self, idx: usize) {
        if idx >= WORKSPACE_COUNT {
            return;
        }

        let out = self.focused_output;
        let current = self.effective_ws(out);
        if idx == current {
            return;
        }

        tracing::debug!("switching to workspace {}", idx + 1);

        if self.config.general.sync_workspaces {
            // Sync mode: every monitor switches its OWN workspace pool to
            // the same slot index. Each monitor still shows its own ws[idx]
            // (different windows), they just track in lockstep.
            self.previous_ws = self.current_ws;
            self.current_ws = idx;
            for o in self.outputs.iter_mut() {
                o.current_ws = idx;
            }
        } else {
            // Independent mode with per-monitor workspace pools: each
            // monitor has its own 9 workspaces. Switching only changes the
            // focused monitor's view — no swap needed because the target
            // workspace is local to this monitor.
            let old_ws = self.outputs[out].current_ws;
            self.previous_ws = old_ws;
            self.outputs[out].current_ws = idx;
            self.current_ws = idx; // mirror for the focused output
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
            for window in self.workspaces[self.focused_output][ws_idx].clients.clone() {
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
        let out = self.focused_output;
        let current = self.effective_ws(out);
        if idx >= WORKSPACE_COUNT || idx == current {
            return;
        }

        let ws = &mut self.workspaces[out][current];
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
                self.pending_sends.push((window, out, idx));
                return;
            }
        }

        // Instant path: remove, unmap, transfer immediately
        ws.remove_client(&window);
        self.space.unmap_elem(&window);
        self.workspaces[out][idx].add_client(window);
        self.apply_layout();
        self.apply_focus();
    }

    // ── Multi-monitor actions ────────────────────────────────────────

    fn do_focus_monitor(&mut self, direction: i32) {
        if self.outputs.len() <= 1 {
            return;
        }
        let new_idx = match step_output(&self.output_order, self.focused_output, direction) {
            Some(i) => i,
            None => return,
        };
        self.focused_output = new_idx;

        // Mouse stays where it is. The user explicitly wants keyboard focus
        // and pointer position to be independent — see `handle_pointer_motion_relative`
        // for the matching change that stops pointer motion from updating
        // focused_output.

        // Repaint every bar so the focused-monitor highlight moves, and
        // re-run layout so each output's border rasterization picks up the
        // new global focus (only the focused monitor shows the accent).
        self.bar_dirty = true;

        self.apply_layout();
        self.apply_focus();
        let name = self.outputs[new_idx].output.name();
        tracing::debug!("focused monitor {new_idx} ({name})");
    }

    fn do_send_monitor(&mut self, direction: i32) {
        if self.outputs.len() <= 1 {
            return;
        }
        let target = match step_output(&self.output_order, self.focused_output, direction) {
            Some(i) => i,
            None => return,
        };

        let src_out = self.focused_output;
        let src_ws = self.effective_ws(src_out);
        let dst_ws = self.effective_ws(target);

        let window = match self.workspaces[src_out][src_ws].focused() {
            Some(w) => w.clone(),
            None => return,
        };

        let src_name = self.outputs[src_out].output.name();
        let dst_name = self.outputs[target].output.name();
        tracing::debug!(
            "sending window {src_name}[ws {}] → {dst_name}[ws {}]",
            src_ws + 1,
            dst_ws + 1,
        );

        self.workspaces[src_out][src_ws].remove_client(&window);
        self.space.unmap_elem(&window);
        self.workspaces[target][dst_ws].add_client(window);

        // The window's new home is the destination monitor; keyboard focus
        // should follow it so the user can keep typing into the same window
        // they just moved.
        self.focused_output = target;

        self.bar_dirty = true;
        self.apply_layout();
        self.apply_focus();
    }

    fn do_toggle_sync(&mut self) {
        let was_sync = self.config.general.sync_workspaces;
        self.config.general.sync_workspaces = !was_sync;

        if self.config.general.sync_workspaces {
            // Entering sync mode: every monitor's pool jumps to the focused
            // output's current workspace slot. Each monitor still owns its
            // own pool — they just share the same slot index.
            let ws = self.outputs.get(self.focused_output)
                .map(|o| o.current_ws)
                .unwrap_or(0);
            self.current_ws = ws;
            for o in self.outputs.iter_mut() {
                o.current_ws = ws;
            }
        } else {
            // Leaving sync mode: each monitor keeps its current_ws value
            // (still per-monitor with isolated pools). Nothing to redistribute.
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
        // Drop every cached named cursor — the previously loaded
        // shapes (text, pointer, resize, etc.) come from the old
        // theme. Without this, swapping cursor themes via kara-beautify
        // only affects the default arrow until the named cache
        // naturally churns, which never happens for icons already
        // observed since startup. kara-gate's wp_cursor_shape_v1
        // dispatch path repopulates the cache on the next hover.
        self.named_cursor_cache.clear();

        let theme_name = self.config.general.cursor_theme
            .as_deref()
            .unwrap_or("default");
        let size = self.config.general.cursor_size as u32;

        match crate::cursor::load_xcursor(theme_name, "default", size) {
            Some(cache) => {
                let (w, h, n_frames) = cache
                    .frames
                    .first()
                    .map(|f| (f.width, f.height, cache.frames.len()))
                    .unwrap_or((0, 0, 0));
                tracing::info!(
                    "loaded cursor: {}x{} ({} frame(s)) from theme '{}'",
                    w, h, n_frames, theme_name,
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
    ///
    /// An entry fires when its `when` condition matches the current set of
    /// connected outputs. Entries with routing hints (`app_id` + `monitor`
    /// and/or `workspace`) are queued in `pending_autostart_routes`; the
    /// corresponding window gets routed to the target (monitor, workspace)
    /// at map-time via `map_new_toplevel`.
    pub fn run_autostart(&mut self) {
        if self.autostart_done {
            return;
        }
        self.autostart_done = true;

        // Snapshot of connected output names for the condition check.
        let connected: std::collections::HashSet<String> = self
            .outputs
            .iter()
            .map(|o| o.output.name())
            .collect();

        // Collect the entries we're actually going to run so we don't hold
        // an immutable borrow on self.config while mutating self.
        let entries_to_run: Vec<kara_config::AutostartEntry> = self
            .config
            .autostart
            .iter()
            .filter(|e| {
                e.condition.required_monitors.iter().all(|m| connected.contains(m))
                    && e.condition
                        .forbidden_monitors
                        .iter()
                        .all(|m| !connected.contains(m))
            })
            .cloned()
            .collect();

        for entry in entries_to_run {
            // Register a pending route BEFORE spawning so the window can't
            // beat us to the map handler (rare but possible on fast spawns).
            if let Some(app_id) = entry.app_id.as_ref() {
                if entry.monitor.is_some() || entry.workspace.is_some() {
                    // `entry.monitor` is 0-based per the config parser but
                    // means "the Nth monitor in the user's config-declared
                    // order". Map through `output_order` so it always points
                    // to the same physical monitor the user wrote down,
                    // regardless of DRM enumeration order.
                    let target_out = entry
                        .monitor
                        .and_then(|m| self.output_order.get(m).copied())
                        .filter(|m| *m < self.outputs.len())
                        .unwrap_or(self.focused_output);
                    let target_ws = entry.workspace.unwrap_or_else(|| self.effective_ws(target_out));
                    self.pending_autostart_routes.push((
                        app_id.clone(),
                        target_out,
                        target_ws,
                        std::time::Instant::now(),
                    ));
                    tracing::info!(
                        "autostart: queued route app_id={app_id:?} → output {target_out} ws {}",
                        target_ws + 1,
                    );
                }
            }

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

/// Move `focused_output` by ±1 step through the config-declared monitor
/// order. Returns the new outputs-index, or `None` if the current focus
/// isn't in the order vec yet (e.g. mid-hot-plug, between
/// `add_output` and `recompute_output_order`).
fn step_output(order: &[usize], current: usize, direction: i32) -> Option<usize> {
    if order.is_empty() {
        return None;
    }
    let pos = order.iter().position(|&i| i == current)?;
    let count = order.len() as i32;
    let next = ((pos as i32 + direction).rem_euclid(count)) as usize;
    Some(order[next])
}

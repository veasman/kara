//! IPC server integration for kara-gate.
//!
//! Handles incoming connections on the Unix socket, reads requests,
//! dispatches them, and sends responses.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;

use kara_ipc::frame::{read_message, write_message};
use kara_ipc::message::{Request, Response, ThemeColors, WindowGeometry};

use crate::state::Gate;

impl Gate {
    fn screenshot_output_path(&self) -> String {
        // Multi-output region captures fire several ScreenshotOutput
        // requests back-to-back. Second-granularity timestamps let two
        // requests collide on the same path — the second render
        // overwrites the first, glimpse loads the file mid-write, and
        // the whole compose step falls apart with "failed to load
        // capture piece". Use nanoseconds + a per-process atomic
        // counter so every screenshot gets a guaranteed-unique path
        // regardless of how fast they arrive or how coarse the clock
        // happens to be.
        use std::sync::atomic::{AtomicU64, Ordering};
        static SCREENSHOT_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SCREENSHOT_SEQ.fetch_add(1, Ordering::Relaxed);

        let dir = dirs::picture_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        std::fs::create_dir_all(&dir).ok();
        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        // Human-readable seconds prefix so the prune-by-age logic in
        // kara-glimpse keeps working, followed by the disambiguator.
        let ts_secs = timestamp_ns / 1_000_000_000;
        let ts_rem = timestamp_ns % 1_000_000_000;
        dir.join(format!("kara-screenshot-{ts_secs}-{ts_rem:09}-{seq}.png"))
            .to_string_lossy()
            .to_string()
    }

    /// Poll the IPC listener for new connections and handle requests.
    /// Called once per frame from the main loop.
    pub fn poll_ipc(&mut self) {
        // Take listener out to avoid borrow conflict with self
        let listener = match self.ipc_listener.take() {
            Some(l) => l,
            None => return,
        };

        // Accept all pending connections (non-blocking)
        let mut streams = Vec::new();
        loop {
            match listener.accept() {
                Ok((stream, _)) => streams.push(stream),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    tracing::error!("IPC accept error: {e}");
                    break;
                }
            }
        }

        // Put listener back before handling (so self is fully available)
        self.ipc_listener = Some(listener);

        for stream in streams {
            self.handle_ipc_connection(stream);
        }

        // Drain any completed async wallpaper decode.
        if let Some(rx) = self.wallpaper_pending.as_ref() {
            match rx.try_recv() {
                Ok(new_wp) => {
                    self.wallpaper_pending = None;
                    if new_wp.is_some() {
                        self.wallpaper = new_wp;
                        // Drop every cache that sampled the old wallpaper
                        // — otherwise a theme switch keeps blurring the
                        // previous theme's background behind the bar and
                        // the theme picker. GPU textures go with the
                        // bytes: keeping a stale texture would have the
                        // compositor uploading the new blur bytes on
                        // cache rebuild but still drawing the old one.
                        self.bar_blur_cache = None;
                        self.bar_blur_texture = None;
                        self.picker_blur_cache = None;
                        self.picker_blur_texture = None;
                    } else {
                        tracing::warn!("async wallpaper decode returned None");
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.wallpaper_pending = None;
                }
            }
        }
    }

    fn handle_ipc_connection(&mut self, stream: UnixStream) {
        let mut reader = BufReader::new(&stream);
        let mut writer = BufWriter::new(&stream);

        let request: Request = match read_message(&mut reader) {
            Ok(req) => req,
            Err(e) => {
                tracing::debug!("IPC read error: {e}");
                return;
            }
        };

        let response = self.handle_ipc_request(request);

        if let Err(e) = write_message(&mut writer, &response) {
            tracing::debug!("IPC write error: {e}");
        }
    }

    fn handle_ipc_request(&mut self, request: Request) -> Response {
        match request {
            Request::GetWorkspaces => {
                // Per-monitor pools: report occupancy for the focused
                // output's pool (the one the user is currently viewing).
                let occupied: Vec<bool> = self
                    .workspaces
                    .get(self.focused_output)
                    .map(|pool| pool.iter().map(|ws| !ws.clients.is_empty()).collect())
                    .unwrap_or_default();
                Response::Workspaces {
                    current: self.current_ws,
                    occupied,
                }
            }

            Request::GetActiveWindow => {
                let ws_opt = self
                    .workspaces
                    .get(self.focused_output)
                    .and_then(|pool| pool.get(self.current_ws));
                let ws = match ws_opt {
                    Some(ws) => ws,
                    None => return Response::ActiveWindow { title: String::new(), app_id: String::new() },
                };
                let (title, app_id) = ws
                    .focused()
                    .and_then(|w| w.toplevel())
                    .map(|t| {
                        smithay::wayland::compositor::with_states(t.wl_surface(), |states| {
                            let data = states
                                .data_map
                                .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                                .and_then(|d| d.lock().ok());
                            let title = data
                                .as_ref()
                                .and_then(|d| d.title.clone())
                                .unwrap_or_default();
                            let app_id = data
                                .as_ref()
                                .and_then(|d| d.app_id.clone())
                                .unwrap_or_default();
                            (title, app_id)
                        })
                    })
                    .unwrap_or_default();
                Response::ActiveWindow { title, app_id }
            }

            Request::GetOutputs => {
                // Report every real output in config-declared navigation
                // order (matches `self.output_order`) so clients that
                // care about which monitor is "first" — kara-veil for
                // primary-output selection, kara-glimpse for compositing
                // all-monitor captures — agree with the keyboard-focus
                // ordering the user configured.
                let order: Vec<usize> = if self.output_order.len() == self.outputs.len() {
                    self.output_order.clone()
                } else {
                    (0..self.outputs.len()).collect()
                };
                let outputs: Vec<kara_ipc::OutputInfo> = order
                    .iter()
                    .filter_map(|&idx| self.outputs.get(idx))
                    .map(|o| {
                        let name = o.output.name();
                        // Walk the user's monitor config to surface the
                        // `primary` marker. Clients like kara-veil
                        // render their login card on whichever output
                        // this flag points at, independent of spatial
                        // position or enumeration order.
                        let primary = self
                            .config
                            .monitors
                            .iter()
                            .find(|m| m.name == name)
                            .map(|m| m.primary)
                            .unwrap_or(false);
                        kara_ipc::OutputInfo {
                            name,
                            width: o.size.0,
                            height: o.size.1,
                            x: o.location.x,
                            y: o.location.y,
                            primary,
                        }
                    })
                    .collect();
                // Diagnostic: surface exactly what GetOutputs is
                // returning so "lock lands on wrong monitor" bugs are
                // easy to triage from the log alone.
                tracing::info!(
                    "ipc GetOutputs: {}",
                    outputs
                        .iter()
                        .map(|o| format!(
                            "{}@({},{}) {}x{}{}",
                            o.name,
                            o.x,
                            o.y,
                            o.width,
                            o.height,
                            if o.primary { " PRIMARY" } else { "" }
                        ))
                        .collect::<Vec<_>>()
                        .join(" | ")
                );
                if outputs.is_empty() {
                    // Winit dev backend or no outputs attached — fall
                    // back to focused-output size under a stub name so
                    // clients still have something to render against.
                    let (w, h) = self.output_size();
                    Response::Outputs {
                        outputs: vec![kara_ipc::OutputInfo {
                            name: "winit".to_string(),
                            width: w,
                            height: h,
                            x: 0,
                            y: 0,
                            primary: true,
                        }],
                    }
                } else {
                    Response::Outputs { outputs }
                }
            }

            Request::GetTheme => {
                let t = &self.config.theme;
                let g = &self.config.general;
                Response::Theme {
                    colors: ThemeColors {
                        bg: t.bg,
                        surface: t.surface,
                        text: t.text,
                        text_muted: t.text_muted,
                        accent: t.accent,
                        accent_soft: t.accent_soft,
                        border: t.border,
                        bar_height: Some(self.config.bar.height as u16),
                        bar_background: Some(self.config.bar.background),
                        bar_background_alpha: Some(self.config.bar.background_alpha),
                        // Surface the theme-driven window-border
                        // decoration so kara-glimpse / kara-whisper
                        // can draw the same chrome as real kara-gate
                        // windows. `border_tile` is only set when
                        // the active theme rasterized an SVG tile.
                        border_px: Some(g.border_px.max(0) as u16),
                        border_radius: Some(g.border_radius.max(0) as u16),
                        border_tile_path: g
                            .border_tile
                            .as_ref()
                            .map(|p| p.to_string_lossy().into_owned()),
                        // Surface `general.font` so tool UIs (whisper,
                        // veil, summon) match the session's typography
                        // instead of whatever cosmic-text picks as a
                        // generic default.
                        font_family: Some(g.font.clone()).filter(|s| !s.is_empty()),
                    },
                }
            }

            Request::ViewWorkspace { index } => {
                self.dispatch_action(crate::actions::Action::ViewWs(index));
                Response::Ok
            }

            Request::SendToWorkspace { index } => {
                self.dispatch_action(crate::actions::Action::SendWs(index));
                Response::Ok
            }

            Request::FocusNext => {
                self.dispatch_action(crate::actions::Action::FocusNext);
                Response::Ok
            }

            Request::FocusPrev => {
                self.dispatch_action(crate::actions::Action::FocusPrev);
                Response::Ok
            }

            Request::KillClient => {
                self.dispatch_action(crate::actions::Action::KillClient);
                Response::Ok
            }

            Request::Reload => {
                self.reload_config();
                Response::Ok
            }

            Request::Spawn { command } => {
                self.dispatch_action(crate::actions::Action::SpawnRaw(command));
                Response::Ok
            }

            Request::ThemeChanged { theme_name } => {
                tracing::info!("IPC: theme changed to '{theme_name}', reloading config");
                self.reload_config();
                Response::Ok
            }

            Request::WallpaperChanged { path } => {
                tracing::info!("IPC: wallpaper changed to '{path}' (decoding async)");
                // Decode on a worker thread so a large GIF doesn't
                // stall the compositor. The main loop keeps showing
                // the current wallpaper; `poll_ipc()` drains the
                // receiver each tick and swaps when the new one is
                // ready.
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let wp = crate::wallpaper::Wallpaper::load(std::path::Path::new(&path));
                    let _ = tx.send(wp);
                });
                self.wallpaper_pending = Some(rx);
                Response::Ok
            }

            Request::Screenshot => {
                // Legacy full-screen capture of the focused output.
                // output_idx=None means "whichever output is focused at
                // render time"; matches pre-queue behaviour.
                let path_str = self.screenshot_output_path();
                self.screenshot_queue.push(crate::state::PendingScreenshot {
                    path: path_str.clone(),
                    output_idx: None,
                    region: None,
                });
                Response::ScreenshotDone { path: path_str }
            }

            Request::ScreenshotRegion { x, y, w, h } => {
                let path_str = self.screenshot_output_path();
                self.screenshot_queue.push(crate::state::PendingScreenshot {
                    path: path_str.clone(),
                    output_idx: None,
                    region: Some((x, y, w, h)),
                });
                Response::ScreenshotDone { path: path_str }
            }

            Request::ScreenshotOutput { name } => {
                // Resolve the connector name to an outputs-index; bail if
                // the caller asked for a monitor kara doesn't know about.
                // Multi-output capture paths (kara-glimpse region
                // selection, kara-veil lock backdrop) fire several of
                // these in rapid succession — queuing them lets every
                // capture land on its own frame instead of having the
                // N-th request silently overwrite the previous N-1.
                match self
                    .outputs
                    .iter()
                    .position(|o| o.output.name() == name)
                {
                    Some(idx) => {
                        let path_str = self.screenshot_output_path();
                        self.screenshot_queue.push(crate::state::PendingScreenshot {
                            path: path_str.clone(),
                            output_idx: Some(idx),
                            region: None,
                        });
                        Response::ScreenshotDone { path: path_str }
                    }
                    None => Response::Error {
                        message: format!("no connected output named '{name}'"),
                    },
                }
            }

            Request::GetWindowGeometries => {
                // Glimpse now runs per-output layer surfaces for interactive
                // region selection and operates in GLOBAL coordinates — all
                // windows across all outputs are candidates. Return geometries
                // in compositor-global coords (no per-output subtraction) so
                // hover/snap targeting works across the full desktop.
                let title_for = |w: &smithay::desktop::Window| -> (String, String) {
                    w.toplevel()
                        .map(|t| {
                            smithay::wayland::compositor::with_states(t.wl_surface(), |states| {
                                let data = states.data_map
                                    .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                                    .and_then(|d| d.lock().ok());
                                let title = data.as_ref()
                                    .and_then(|d| d.title.clone())
                                    .unwrap_or_default();
                                let app_id = data.as_ref()
                                    .and_then(|d| d.app_id.clone())
                                    .unwrap_or_default();
                                (title, app_id)
                            })
                        })
                        .unwrap_or_default()
                };

                let mut windows: Vec<WindowGeometry> = Vec::new();

                // Walk every output. A fullscreen window on any output is
                // the ONLY candidate on that output; otherwise use the
                // tiled/floating layout of that output's effective workspace.
                for out_idx in 0..self.outputs.len() {
                    let out = match self.outputs.get(out_idx) {
                        Some(o) => o,
                        None => continue,
                    };
                    let out_loc = (out.location.x, out.location.y);
                    let out_size = out.size;

                    if let Some(fs_win) = out.fullscreen_window.as_ref() {
                        let (title, app_id) = title_for(fs_win);
                        windows.push(WindowGeometry {
                            app_id,
                            title,
                            x: out_loc.0,
                            y: out_loc.1,
                            w: out_size.0,
                            h: out_size.1,
                        });
                        continue;
                    }

                    let ws_idx = self.effective_ws(out_idx);
                    if let Some(ws) = self.workspaces.get(out_idx).and_then(|pool| pool.get(ws_idx)) {
                        let area = out.workarea;
                        let geos = crate::layout::layout_workspace(
                            ws,
                            area,
                            self.config.general.border_px,
                        );
                        for g in geos.iter().filter(|g| g.visible) {
                            let (title, app_id) = title_for(&g.window);
                            windows.push(WindowGeometry {
                                app_id,
                                title,
                                x: g.rect.loc.x,
                                y: g.rect.loc.y,
                                w: g.rect.size.w,
                                h: g.rect.size.h,
                            });
                        }
                    }

                    // Currently-visible scratchpad windows on this output.
                    for sp in self
                        .scratchpads
                        .iter()
                        .filter(|sp| sp.visible && !sp.hiding && sp.output_idx == out_idx)
                    {
                        for client in sp.workspace.clients.iter() {
                            let geo = client.geometry();
                            if let Some(loc) = self.space.element_location(client) {
                                let (title, app_id) = title_for(client);
                                windows.push(WindowGeometry {
                                    app_id,
                                    title,
                                    x: loc.x,
                                    y: loc.y,
                                    w: geo.size.w,
                                    h: geo.size.h,
                                });
                            }
                        }
                    }
                }

                Response::WindowGeometries { windows }
            }

            Request::Subscribe | Request::Unsubscribe => {
                // TODO: event subscription
                Response::Ok
            }
        }
    }
}

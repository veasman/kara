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
        let dir = dirs::picture_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        std::fs::create_dir_all(&dir).ok();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        dir.join(format!("kara-screenshot-{timestamp}.png"))
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
                let (w, h) = self.output_size();
                Response::Outputs {
                    outputs: vec![kara_ipc::OutputInfo {
                        name: "winit".to_string(),
                        width: w,
                        height: h,
                    }],
                }
            }

            Request::GetTheme => {
                let t = &self.config.theme;
                Response::Theme {
                    colors: ThemeColors {
                        bg: t.bg,
                        surface: t.surface,
                        text: t.text,
                        text_muted: t.text_muted,
                        accent: t.accent,
                        accent_soft: t.accent_soft,
                        border: t.border,
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
                let path_str = self.screenshot_output_path();
                self.screenshot_path = Some(path_str.clone());
                Response::ScreenshotDone { path: path_str }
            }

            Request::ScreenshotRegion { x, y, w, h } => {
                let path_str = self.screenshot_output_path();
                self.screenshot_path = Some(path_str.clone());
                self.screenshot_region = Some((x, y, w, h));
                Response::ScreenshotDone { path: path_str }
            }

            Request::GetWindowGeometries => {
                let out = self.focused_output;
                let ws_idx = self.effective_ws(out);
                let ws = match self.workspaces.get(out).and_then(|pool| pool.get(ws_idx)) {
                    Some(ws) => ws,
                    None => return Response::WindowGeometries { windows: Vec::new() },
                };
                let area = self.workarea();
                let geos = crate::layout::layout_workspace(ws, area, self.config.general.border_px);
                let windows: Vec<WindowGeometry> = geos.iter()
                    .filter(|g| g.visible)
                    .map(|g| {
                        let (title, app_id) = g.window.toplevel()
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
                            .unwrap_or_default();
                        WindowGeometry {
                            app_id,
                            title,
                            x: g.rect.loc.x,
                            y: g.rect.loc.y,
                            w: g.rect.size.w,
                            h: g.rect.size.h,
                        }
                    })
                    .collect();
                Response::WindowGeometries { windows }
            }

            Request::Subscribe | Request::Unsubscribe => {
                // TODO: event subscription
                Response::Ok
            }
        }
    }
}

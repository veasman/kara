use kara_ipc::WindowGeometry;

pub enum HoverTarget {
    /// "No window under the pointer" — full-monitor preview. Carries
    /// the focused output's global-coord top-left corner so the
    /// highlight rect lands on the right monitor when the desktop
    /// spans multiple outputs. A plain `(w, h)` would anchor at global
    /// `(0, 0)` and slide the preview off the left edge of anything
    /// that isn't the leftmost monitor.
    Fullscreen {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    },
    Window {
        /// Index into the window geometry vec from the compositor. Held
        /// so a future "capture this specific window by ID" IPC can
        /// resolve back to the same window even if z-order shifts.
        #[allow(dead_code)]
        index: usize,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    },
}

pub enum Mode {
    Hover,
    Dragging { start_x: f64, start_y: f64 },
}

pub struct SelectionState {
    pub target: HoverTarget,
    pub mode: Mode,
    pub pointer: (f64, f64),
    /// Top-left of the "fullscreen" rect in global coords — equals the
    /// focused output's origin. Paired with screen_w/h to drive the
    /// `Fullscreen` hover target.
    pub screen_x: i32,
    pub screen_y: i32,
    pub screen_w: i32,
    pub screen_h: i32,
}

impl SelectionState {
    pub fn new(screen_x: i32, screen_y: i32, screen_w: i32, screen_h: i32) -> Self {
        Self {
            target: HoverTarget::Fullscreen {
                x: screen_x,
                y: screen_y,
                w: screen_w,
                h: screen_h,
            },
            mode: Mode::Hover,
            pointer: (0.0, 0.0),
            screen_x,
            screen_y,
            screen_w,
            screen_h,
        }
    }

    pub fn update_hover(&mut self, windows: &[WindowGeometry]) {
        let px = self.pointer.0 as i32;
        let py = self.pointer.1 as i32;

        for (i, win) in windows.iter().enumerate() {
            if px >= win.x && px < win.x + win.w && py >= win.y && py < win.y + win.h {
                self.target = HoverTarget::Window {
                    index: i,
                    x: win.x,
                    y: win.y,
                    w: win.w,
                    h: win.h,
                };
                return;
            }
        }

        self.target = HoverTarget::Fullscreen {
            x: self.screen_x,
            y: self.screen_y,
            w: self.screen_w,
            h: self.screen_h,
        };
    }

    pub fn start_press(&mut self) {
        self.mode = Mode::Dragging {
            start_x: self.pointer.0,
            start_y: self.pointer.1,
        };
    }

    pub fn end_press(&self) -> (i32, i32, i32, i32) {
        match &self.mode {
            Mode::Dragging { start_x, start_y } => {
                let dx = (self.pointer.0 - start_x).abs();
                let dy = (self.pointer.1 - start_y).abs();
                if dx < 5.0 && dy < 5.0 {
                    // Click — use hover target
                    self.hover_rect()
                } else {
                    // Drag — use selection rectangle
                    let x = start_x.min(self.pointer.0) as i32;
                    let y = start_y.min(self.pointer.1) as i32;
                    let w = dx as i32;
                    let h = dy as i32;
                    (x, y, w.max(1), h.max(1))
                }
            }
            Mode::Hover => self.hover_rect(),
        }
    }

    fn hover_rect(&self) -> (i32, i32, i32, i32) {
        match &self.target {
            HoverTarget::Window { x, y, w, h, .. } => (*x, *y, *w, *h),
            HoverTarget::Fullscreen { x, y, w, h } => (*x, *y, *w, *h),
        }
    }

    /// Returns the current visual highlight rect for rendering.
    pub fn highlight_rect(&self) -> (i32, i32, i32, i32) {
        match &self.mode {
            Mode::Dragging { start_x, start_y } => {
                let x = start_x.min(self.pointer.0);
                let y = start_y.min(self.pointer.1);
                let w = (self.pointer.0 - start_x).abs();
                let h = (self.pointer.1 - start_y).abs();
                (x as i32, y as i32, w as i32, h as i32)
            }
            Mode::Hover => self.hover_rect(),
        }
    }
}

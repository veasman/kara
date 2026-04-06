use kara_ipc::WindowGeometry;

pub enum HoverTarget {
    None,
    Fullscreen { w: i32, h: i32 },
    Window { index: usize, x: i32, y: i32, w: i32, h: i32 },
}

pub enum Mode {
    Hover,
    Dragging { start_x: f64, start_y: f64 },
}

pub struct SelectionState {
    pub target: HoverTarget,
    pub mode: Mode,
    pub pointer: (f64, f64),
    pub screen_w: i32,
    pub screen_h: i32,
}

impl SelectionState {
    pub fn new(screen_w: i32, screen_h: i32) -> Self {
        Self {
            target: HoverTarget::None,
            mode: Mode::Hover,
            pointer: (0.0, 0.0),
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
            HoverTarget::Fullscreen { w, h } => (0, 0, *w, *h),
            HoverTarget::None => (0, 0, self.screen_w, self.screen_h),
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

use smithay::desktop::Window;

pub const WORKSPACE_COUNT: usize = 9;
pub const MFACT_STEP: f32 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    Tile,
    Monocle,
}

pub struct Workspace {
    pub id: usize,
    pub layout: LayoutKind,
    pub mfact: f32,
    pub nmaster: usize,
    pub gap_px: i32,
    pub clients: Vec<Window>,
    pub focused_idx: Option<usize>,
    pub last_focused_idx: Option<usize>,
}

impl Workspace {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            layout: LayoutKind::Tile,
            mfact: 0.5,
            nmaster: 1,
            gap_px: 8,
            clients: Vec::new(),
            focused_idx: None,
            last_focused_idx: None,
        }
    }

    pub fn focused(&self) -> Option<&Window> {
        self.focused_idx.and_then(|i| self.clients.get(i))
    }

    pub fn focused_mut(&mut self) -> Option<&Window> {
        self.focused_idx.and_then(|i| self.clients.get(i))
    }

    pub fn add_client(&mut self, window: Window) {
        self.clients.push(window);
        let idx = self.clients.len() - 1;
        // Save previous focus as last_focused
        if let Some(old) = self.focused_idx {
            self.last_focused_idx = Some(old);
        }
        self.focused_idx = Some(idx);
    }

    pub fn remove_client(&mut self, window: &Window) -> bool {
        let Some(pos) = self.clients.iter().position(|c| c == window) else {
            return false;
        };

        let was_focused = self.focused_idx == Some(pos);
        self.clients.remove(pos);

        // Fix up indices after removal
        if let Some(ref mut fi) = self.focused_idx {
            if *fi == pos {
                // Was focused — try last_focused first, then neighbor
                let fallback = self.last_focused_idx
                    .filter(|&i| i != pos && i < self.clients.len() + 1)
                    .map(|i| if i > pos { i - 1 } else { i })
                    .or_else(|| if !self.clients.is_empty() {
                        Some(pos.min(self.clients.len() - 1))
                    } else {
                        None
                    });
                self.focused_idx = fallback;
                self.last_focused_idx = None;
            } else if *fi > pos {
                *fi -= 1;
            }
        }

        // Fix last_focused_idx
        if let Some(ref mut lfi) = self.last_focused_idx {
            if *lfi == pos {
                self.last_focused_idx = None;
            } else if *lfi > pos {
                *lfi -= 1;
            }
        }

        was_focused
    }

    pub fn focus_next(&mut self) {
        if self.clients.is_empty() {
            return;
        }
        let cur = self.focused_idx.unwrap_or(0);
        let next = (cur + 1) % self.clients.len();
        self.last_focused_idx = self.focused_idx;
        self.focused_idx = Some(next);
    }

    pub fn focus_prev(&mut self) {
        if self.clients.is_empty() {
            return;
        }
        let cur = self.focused_idx.unwrap_or(0);
        let prev = if cur == 0 { self.clients.len() - 1 } else { cur - 1 };
        self.last_focused_idx = self.focused_idx;
        self.focused_idx = Some(prev);
    }

    pub fn zoom_master(&mut self) {
        if self.clients.len() < 2 {
            return;
        }
        let cur = match self.focused_idx {
            Some(i) if i > 0 => i,
            _ => return,
        };
        self.clients.swap(0, cur);
        self.last_focused_idx = self.focused_idx;
        self.focused_idx = Some(0);
    }

    pub fn toggle_layout(&mut self) {
        self.layout = match self.layout {
            LayoutKind::Tile => LayoutKind::Monocle,
            LayoutKind::Monocle => LayoutKind::Tile,
        };
    }

    pub fn adjust_mfact(&mut self, delta: f32) {
        self.mfact = (self.mfact + delta).clamp(0.05, 0.95);
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}

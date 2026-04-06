//! Animation engine — preset-based window transitions.
//!
//! Manages in-flight animations and computes per-frame position offsets.
//! Presets define motion style; duration is the single user-facing knob.

use std::time::{Duration, Instant};

use kara_config::AnimationPreset;
use smithay::desktop::Window;

// ── Easing functions ───────────────────────────────────────────────

/// Ease-out quad: gentle deceleration. Used by clean.
fn ease_out_quad(t: f64) -> f64 {
    t * (2.0 - t)
}

/// Ease-out back: overshoots past target then settles. Bouncy pop feel.
fn ease_out_back(t: f64) -> f64 {
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

fn easing_for_preset(preset: AnimationPreset) -> fn(f64) -> f64 {
    match preset {
        AnimationPreset::Swoosh => ease_out_back,
        AnimationPreset::Clean => ease_out_quad,
        AnimationPreset::Instant => |t| t,
    }
}

// ── Offset computation ─────────────────────────────────────────────

/// Workspace switch direction (determines slide direction).
#[derive(Debug, Clone, Copy)]
pub enum SlideDirection {
    /// Determine direction from window position relative to workarea.
    Auto,
    /// Slide from/toward the left edge.
    Left,
    /// Slide from/toward the right edge.
    Right,
}

/// Compute the starting offset for a window-in animation.
fn compute_in_offset(
    preset: AnimationPreset,
    window_x: i32,
    window_y: i32,
    window_w: i32,
    window_h: i32,
    area_x: i32,
    area_y: i32,
    area_w: i32,
    area_h: i32,
    direction: SlideDirection,
) -> (f64, f64) {
    match preset {
        AnimationPreset::Clean => {
            // Clean = fade only. Real opacity requires GL shaders (future).
            // For now, no positional offset — effectively instant.
            (0.0, 0.0)
        }
        AnimationPreset::Swoosh => {
            // Bouncy pop — moderate slide with overshoot from easing
            let slide = 60.0;
            match direction {
                SlideDirection::Left => (-slide, 0.0),
                SlideDirection::Right => (slide, 0.0),
                SlideDirection::Auto => {
                    let cx = window_x + window_w / 2;
                    let cy = window_y + window_h / 2;
                    let acx = area_x + area_w / 2;
                    let acy = area_y + area_h / 2;
                    let dx = if cx < acx { -slide } else { slide };
                    let dy = if cy < acy { -slide * 0.5 } else { slide * 0.5 };
                    (dx, dy)
                }
            }
        }
        AnimationPreset::Instant => (0.0, 0.0),
    }
}

// ── Active animation ───────────────────────────────────────────────

pub struct ActiveAnimation {
    pub window: Window,
    start: Instant,
    duration: Duration,
    from_offset: (f64, f64),
    to_offset: (f64, f64),
    easing: fn(f64) -> f64,
    /// If true, the window should be unmapped when this animation completes.
    pub unmap_on_complete: bool,
}

impl ActiveAnimation {
    /// Current interpolated offset.
    pub fn current_offset(&self) -> (f64, f64) {
        let elapsed = self.start.elapsed().as_secs_f64();
        let total = self.duration.as_secs_f64();
        if total <= 0.0 {
            return self.to_offset;
        }
        let t = (elapsed / total).clamp(0.0, 1.0);
        let eased = (self.easing)(t);
        let dx = self.from_offset.0 + (self.to_offset.0 - self.from_offset.0) * eased;
        let dy = self.from_offset.1 + (self.to_offset.1 - self.from_offset.1) * eased;
        (dx, dy)
    }

    fn is_complete(&self) -> bool {
        self.start.elapsed() >= self.duration
    }
}

// ── Animation manager ──────────────────────────────────────────────

pub struct AnimationManager {
    pub active: Vec<ActiveAnimation>,
}

impl AnimationManager {
    pub fn new() -> Self {
        Self { active: Vec::new() }
    }

    /// Start a "window in" animation (spawn, receive from ws, unscratchpad).
    pub fn animate_in(
        &mut self,
        window: Window,
        preset: AnimationPreset,
        duration_ms: u32,
        window_x: i32, window_y: i32, window_w: i32, window_h: i32,
        area_x: i32, area_y: i32, area_w: i32, area_h: i32,
        direction: SlideDirection,
    ) {
        if preset == AnimationPreset::Instant || duration_ms == 0 {
            return;
        }
        // Cancel any existing animation on this window
        self.cancel(&window);

        let from_offset = compute_in_offset(
            preset, window_x, window_y, window_w, window_h,
            area_x, area_y, area_w, area_h, direction,
        );

        self.active.push(ActiveAnimation {
            window,
            start: Instant::now(),
            duration: Duration::from_millis(duration_ms as u64),
            from_offset,
            to_offset: (0.0, 0.0),
            easing: easing_for_preset(preset),
            unmap_on_complete: false,
        });
    }

    /// Start a "window out" animation without auto-unmap.
    /// Used for scratchpad hide where batch cleanup is needed.
    pub fn animate_out_no_unmap(
        &mut self,
        window: Window,
        preset: AnimationPreset,
        duration_ms: u32,
        window_x: i32, window_y: i32, window_w: i32, window_h: i32,
        area_x: i32, area_y: i32, area_w: i32, area_h: i32,
        direction: SlideDirection,
    ) {
        if preset == AnimationPreset::Instant || duration_ms == 0 {
            return;
        }
        self.cancel(&window);
        let target_offset = compute_in_offset(
            preset, window_x, window_y, window_w, window_h,
            area_x, area_y, area_w, area_h, direction,
        );
        self.active.push(ActiveAnimation {
            window,
            start: Instant::now(),
            duration: Duration::from_millis(duration_ms as u64),
            from_offset: (0.0, 0.0),
            to_offset: target_offset,
            easing: easing_for_preset(preset),
            unmap_on_complete: false,
        });
    }

    /// Start a "window out" animation (send to ws).
    /// The window will be unmapped when the animation completes.
    pub fn animate_out(
        &mut self,
        window: Window,
        preset: AnimationPreset,
        duration_ms: u32,
        window_x: i32, window_y: i32, window_w: i32, window_h: i32,
        area_x: i32, area_y: i32, area_w: i32, area_h: i32,
        direction: SlideDirection,
    ) {
        if preset == AnimationPreset::Instant || duration_ms == 0 {
            return;
        }
        self.cancel(&window);

        // Out is the reverse of in: start at (0,0), slide to off-screen
        let target_offset = compute_in_offset(
            preset, window_x, window_y, window_w, window_h,
            area_x, area_y, area_w, area_h, direction,
        );

        self.active.push(ActiveAnimation {
            window,
            start: Instant::now(),
            duration: Duration::from_millis(duration_ms as u64),
            from_offset: (0.0, 0.0),
            to_offset: target_offset,
            easing: easing_for_preset(preset),
            unmap_on_complete: true,
        });
    }

    /// Get current position offset for a window, or None if not animating.
    pub fn offset_for(&self, window: &Window) -> Option<(f64, f64)> {
        self.active
            .iter()
            .find(|a| &a.window == window)
            .map(|a| a.current_offset())
    }

    /// Tick: remove completed animations. Returns windows that need unmapping.
    pub fn tick(&mut self) -> Vec<Window> {
        let mut to_unmap = Vec::new();
        self.active.retain(|a| {
            if a.is_complete() {
                if a.unmap_on_complete {
                    to_unmap.push(a.window.clone());
                }
                false
            } else {
                true
            }
        });
        to_unmap
    }

    /// Whether any animations are currently active.
    pub fn has_active(&self) -> bool {
        !self.active.is_empty()
    }

    /// Cancel all animations for a window.
    pub fn cancel(&mut self, window: &Window) {
        self.active.retain(|a| &a.window != window);
    }
}

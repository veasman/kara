use smithay::desktop::Window;
use smithay::utils::{Logical, Rectangle};

use crate::workspace::{LayoutKind, Workspace};

/// Geometry assignment for a single client
pub struct ClientGeometry {
    pub window: Window,
    /// The inner rect (where the window content goes, inset by border)
    pub rect: Rectangle<i32, Logical>,
    /// The outer rect (border area). If border_px > 0, this is larger than rect.
    pub border_rect: Option<Rectangle<i32, Logical>>,
    pub visible: bool,
    pub is_focused: bool,
}

/// Calculate geometry for all clients in a workspace.
///
/// Borders render **outside** the window's visible content rect — the
/// window gets its full requested (floating) or tile-slot (tiled)
/// size, and the border extends outward. The layout reserves a
/// `border_px` margin inside the work area for the outer borders of
/// edge tiles, and grows the inter-tile gap to `max(gap_px, 2*border_px)`
/// so adjacent tile borders don't overlap. This means a wider border
/// never eats into window content — it only eats into the workarea
/// margins and the gap space between tiles.
///
/// `border_rect` (when Some) is the outer rectangle drawn by the
/// border renderer; `rect` is the window content area and equals the
/// tile slot / floating window size exactly.
pub fn layout_workspace(
    ws: &Workspace,
    area: Rectangle<i32, Logical>,
    border_px: i32,
) -> Vec<ClientGeometry> {
    if ws.clients.is_empty() {
        return vec![];
    }

    let focused_idx = ws.focused_idx;

    // Reserve an outer margin equal to border_px on every side of the
    // workarea so edge tiles' outward-extending borders stay inside
    // the monitor. Effective gap grows to 2*border_px so adjacent
    // tiles' borders don't overlap at the midpoint.
    let padded_area = inset_rect(area, border_px);
    let effective_gap = ws.gap_px.max(border_px * 2);

    // Separate tiled and floating
    let tiled_indices = ws.tiled_indices();

    // Layout tiled windows
    let mut result = match ws.layout {
        LayoutKind::Tile => layout_tile_indexed(
            ws,
            &tiled_indices,
            padded_area,
            effective_gap,
            border_px,
            focused_idx,
        ),
        LayoutKind::Monocle => layout_monocle_indexed(
            ws,
            &tiled_indices,
            padded_area,
            effective_gap,
            border_px,
            focused_idx,
        ),
    };

    // Layout floating windows — honor client-requested size, centered in the work area.
    // Clamp to (padded_area * 0.95) to leave a visible margin; fall back to 640x480 if
    // the client has not yet committed a geometry (pre-first-buffer). The border
    // extends outward from the window rect, not into it.
    let max_w = (padded_area.size.w as f32 * 0.95) as i32;
    let max_h = (padded_area.size.h as f32 * 0.95) as i32;
    for (i, client) in ws.clients.iter().enumerate() {
        if !ws.is_floating(i) {
            continue;
        }
        let requested = client.geometry().size;
        let rw = if requested.w > 1 { requested.w } else { 640 };
        let rh = if requested.h > 1 { requested.h } else { 480 };
        let fw = rw.min(max_w).max(1);
        let fh = rh.min(max_h).max(1);
        let fx = padded_area.loc.x + (padded_area.size.w - fw) / 2;
        let fy = padded_area.loc.y + (padded_area.size.h - fh) / 2;

        let window_rect = Rectangle::new((fx, fy).into(), (fw, fh).into());
        let outer = outset_rect(window_rect, border_px);

        result.push(ClientGeometry {
            window: client.clone(),
            rect: window_rect,
            border_rect: if border_px > 0 { Some(outer) } else { None },
            visible: true,
            is_focused: focused_idx == Some(i),
        });
    }

    result
}

/// Inset a rectangle by `px` on all sides.
fn inset_rect(r: Rectangle<i32, Logical>, px: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (r.loc.x + px, r.loc.y + px).into(),
        ((r.size.w - px * 2).max(1), (r.size.h - px * 2).max(1)).into(),
    )
}

/// Outset a rectangle by `px` on all sides (the inverse of inset).
fn outset_rect(r: Rectangle<i32, Logical>, px: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (r.loc.x - px, r.loc.y - px).into(),
        ((r.size.w + px * 2).max(1), (r.size.h + px * 2).max(1)).into(),
    )
}

fn layout_tile_indexed(
    ws: &Workspace,
    indices: &[usize],
    area: Rectangle<i32, Logical>,
    gap: i32,
    border_px: i32,
    focused_idx: Option<usize>,
) -> Vec<ClientGeometry> {
    let count = indices.len();

    if count == 0 {
        return vec![];
    }

    // Apply outer gaps
    let area = Rectangle::new(
        (area.loc.x + gap, area.loc.y + gap).into(),
        (area.size.w - gap * 2, area.size.h - gap * 2).into(),
    );

    if count == 1 {
        let tile = area;
        let border_rect = outset_rect(tile, border_px);
        return vec![ClientGeometry {
            window: ws.clients[indices[0]].clone(),
            rect: tile,
            border_rect: if border_px > 0 { Some(border_rect) } else { None },
            visible: true,
            is_focused: focused_idx == Some(indices[0]),
        }];
    }

    let nmaster = ws.nmaster.min(count);
    let master_w = if count > nmaster {
        (area.size.w as f32 * ws.mfact) as i32
    } else {
        area.size.w
    };
    let stack_w = area.size.w - master_w - gap;

    let mut result = Vec::with_capacity(count);

    // Master area
    let master_h_each = if nmaster > 0 {
        (area.size.h - gap * (nmaster as i32 - 1)) / nmaster as i32
    } else {
        area.size.h
    };

    let mut mi = 0;
    let mut si = 0;
    let stack_count = count - nmaster;
    let stack_h_each = if stack_count > 0 {
        (area.size.h - gap * (stack_count as i32 - 1)) / stack_count as i32
    } else {
        area.size.h
    };

    for (pos, &client_idx) in indices.iter().enumerate() {
        let tile = if pos < nmaster {
            let r = Rectangle::new(
                (area.loc.x, area.loc.y + (master_h_each + gap) * mi).into(),
                (master_w, master_h_each).into(),
            );
            mi += 1;
            r
        } else {
            let r = Rectangle::new(
                (area.loc.x + master_w + gap, area.loc.y + (stack_h_each + gap) * si).into(),
                (stack_w.max(0), stack_h_each).into(),
            );
            si += 1;
            r
        };

        let border_rect = outset_rect(tile, border_px);

        result.push(ClientGeometry {
            window: ws.clients[client_idx].clone(),
            rect: tile,
            border_rect: if border_px > 0 { Some(border_rect) } else { None },
            visible: true,
            is_focused: focused_idx == Some(client_idx),
        });
    }

    result
}

fn layout_monocle_indexed(
    ws: &Workspace,
    indices: &[usize],
    area: Rectangle<i32, Logical>,
    gap: i32,
    border_px: i32,
    focused_idx: Option<usize>,
) -> Vec<ClientGeometry> {
    let area = Rectangle::new(
        (area.loc.x + gap, area.loc.y + gap).into(),
        (area.size.w - gap * 2, area.size.h - gap * 2).into(),
    );

    let fi = focused_idx.unwrap_or(0);

    indices
        .iter()
        .map(|&client_idx| {
            let tile = area;
            let border_rect = outset_rect(tile, border_px);
            ClientGeometry {
                window: ws.clients[client_idx].clone(),
                rect: tile,
                border_rect: if border_px > 0 { Some(border_rect) } else { None },
                visible: client_idx == fi,
                is_focused: client_idx == fi,
            }
        })
        .collect()
}

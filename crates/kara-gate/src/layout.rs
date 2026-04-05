use smithay::desktop::Window;
use smithay::utils::{Logical, Rectangle, Size};

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
/// Floating windows get centered geometry; tiled windows go through tile/monocle.
/// border_px: the border width in pixels (from config). Each window's rect is inset
/// by this amount, and border_rect gives the outer area for drawing borders.
pub fn layout_workspace(
    ws: &Workspace,
    area: Rectangle<i32, Logical>,
    border_px: i32,
) -> Vec<ClientGeometry> {
    if ws.clients.is_empty() {
        return vec![];
    }

    let focused_idx = ws.focused_idx;

    // Separate tiled and floating
    let tiled_indices = ws.tiled_indices();

    // Layout tiled windows
    let mut result = match ws.layout {
        LayoutKind::Tile => layout_tile_indexed(ws, &tiled_indices, area, border_px, focused_idx),
        LayoutKind::Monocle => layout_monocle_indexed(ws, &tiled_indices, area, border_px, focused_idx),
    };

    // Layout floating windows — centered in the work area
    for (i, client) in ws.clients.iter().enumerate() {
        if !ws.is_floating(i) {
            continue;
        }
        // Default floating size: 60% of area
        let fw = (area.size.w as f32 * 0.6) as i32;
        let fh = (area.size.h as f32 * 0.6) as i32;
        let fx = area.loc.x + (area.size.w - fw) / 2;
        let fy = area.loc.y + (area.size.h - fh) / 2;

        let outer = Rectangle::from_loc_and_size((fx, fy), (fw, fh));
        let inner = inset_rect(outer, border_px);

        result.push(ClientGeometry {
            window: client.clone(),
            rect: inner,
            border_rect: if border_px > 0 { Some(outer) } else { None },
            visible: true,
            is_focused: focused_idx == Some(i),
        });
    }

    result
}

/// Inset a rectangle by `px` on all sides.
fn inset_rect(r: Rectangle<i32, Logical>, px: i32) -> Rectangle<i32, Logical> {
    Rectangle::from_loc_and_size(
        (r.loc.x + px, r.loc.y + px),
        ((r.size.w - px * 2).max(1), (r.size.h - px * 2).max(1)),
    )
}

fn layout_tile_indexed(
    ws: &Workspace,
    indices: &[usize],
    area: Rectangle<i32, Logical>,
    border_px: i32,
    focused_idx: Option<usize>,
) -> Vec<ClientGeometry> {
    let gap = ws.gap_px;
    let count = indices.len();

    if count == 0 {
        return vec![];
    }

    // Apply outer gaps
    let area = Rectangle::from_loc_and_size(
        (area.loc.x + gap, area.loc.y + gap),
        (area.size.w - gap * 2, area.size.h - gap * 2),
    );

    if count == 1 {
        let outer = area;
        let inner = inset_rect(outer, border_px);
        return vec![ClientGeometry {
            window: ws.clients[indices[0]].clone(),
            rect: inner,
            border_rect: if border_px > 0 { Some(outer) } else { None },
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
        let outer = if pos < nmaster {
            let r = Rectangle::from_loc_and_size(
                (area.loc.x, area.loc.y + (master_h_each + gap) * mi),
                (master_w, master_h_each),
            );
            mi += 1;
            r
        } else {
            let r = Rectangle::from_loc_and_size(
                (area.loc.x + master_w + gap, area.loc.y + (stack_h_each + gap) * si),
                (stack_w.max(0), stack_h_each),
            );
            si += 1;
            r
        };

        let inner = inset_rect(outer, border_px);

        result.push(ClientGeometry {
            window: ws.clients[client_idx].clone(),
            rect: inner,
            border_rect: if border_px > 0 { Some(outer) } else { None },
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
    border_px: i32,
    focused_idx: Option<usize>,
) -> Vec<ClientGeometry> {
    let gap = ws.gap_px;
    let area = Rectangle::from_loc_and_size(
        (area.loc.x + gap, area.loc.y + gap),
        (area.size.w - gap * 2, area.size.h - gap * 2),
    );

    let fi = focused_idx.unwrap_or(0);

    indices
        .iter()
        .map(|&client_idx| {
            let outer = area;
            let inner = inset_rect(outer, border_px);
            ClientGeometry {
                window: ws.clients[client_idx].clone(),
                rect: inner,
                border_rect: if border_px > 0 { Some(outer) } else { None },
                visible: client_idx == fi,
                is_focused: client_idx == fi,
            }
        })
        .collect()
}

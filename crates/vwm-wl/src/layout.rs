use smithay::desktop::Window;
use smithay::utils::{Logical, Rectangle, Size};

use crate::workspace::{LayoutKind, Workspace};

/// Geometry assignment for a single client
pub struct ClientGeometry {
    pub window: Window,
    pub rect: Rectangle<i32, Logical>,
    pub visible: bool,
}

/// Calculate geometry for all clients in a workspace
pub fn layout_workspace(
    ws: &Workspace,
    area: Rectangle<i32, Logical>,
) -> Vec<ClientGeometry> {
    if ws.clients.is_empty() {
        return vec![];
    }

    match ws.layout {
        LayoutKind::Tile => layout_tile(ws, area),
        LayoutKind::Monocle => layout_monocle(ws, area),
    }
}

fn layout_tile(ws: &Workspace, area: Rectangle<i32, Logical>) -> Vec<ClientGeometry> {
    let gap = ws.gap_px;
    let clients = &ws.clients;
    let count = clients.len();

    if count == 0 {
        return vec![];
    }

    // Apply outer gaps
    let area = Rectangle::from_loc_and_size(
        (area.loc.x + gap, area.loc.y + gap),
        (area.size.w - gap * 2, area.size.h - gap * 2),
    );

    if count == 1 {
        return vec![ClientGeometry {
            window: clients[0].clone(),
            rect: area,
            visible: true,
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

    for (i, client) in clients.iter().enumerate() {
        let rect = if i < nmaster {
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

        result.push(ClientGeometry {
            window: client.clone(),
            rect,
            visible: true,
        });
    }

    result
}

fn layout_monocle(ws: &Workspace, area: Rectangle<i32, Logical>) -> Vec<ClientGeometry> {
    let gap = ws.gap_px;
    let area = Rectangle::from_loc_and_size(
        (area.loc.x + gap, area.loc.y + gap),
        (area.size.w - gap * 2, area.size.h - gap * 2),
    );

    let focused_idx = ws.focused_idx.unwrap_or(0);

    ws.clients
        .iter()
        .enumerate()
        .map(|(i, client)| ClientGeometry {
            window: client.clone(),
            rect: area,
            visible: i == focused_idx,
        })
        .collect()
}

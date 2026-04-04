# CLAUDE.md

This file provides guidance to Claude Code when working with code in this repository.

## Project

vwm-wl is a Wayland compositor written in Rust using smithay. It is the successor to vwm (X11 tiling WM in C11, at `~/repos/vwm/`). Not a port — a ground-up rewrite carrying vwm's layout philosophy (tile/monocle/float, 9-workspace model, scratchpad overlay, bar). The sole developer is Charlton / veasman. Target platform: Artix Linux (Arch-based).

## Build Commands

```bash
cargo build              # Build debug binary
cargo build --release    # Build release binary
cargo check              # Type-check without building
cargo run                # Run with winit backend (inside current X session)
```

No tests yet. No CI yet. Binary is `target/debug/vwm-wl` (or `target/release/vwm-wl`).

## Architecture

**Workspace model**: Cargo workspace with three crates:
- `crates/vwm-wl/` — the compositor binary
- `crates/vwm-config/` — config parsing library (stub, M3)
- `crates/vwm-bar/` — bar rendering library (stub, M4)

**Compositor state**: `Vwm` struct in `state.rs` holds all smithay protocol state, the workspace model, keybinds, and the smithay `Space<Window>` used for rendering.

**Backend**: Currently winit-only (development backend, runs inside X11). Production backend (udev/DRM + libinput) planned for M7.

### Key modules (`crates/vwm-wl/src/`)

| Module | Role |
|--------|------|
| `main.rs` | Entry point, event loop, winit backend setup, render loop |
| `state.rs` | `Vwm` struct, smithay handler impls (Compositor, XdgShell, Seat, Shm, DataDevice, Output), `apply_layout()`, `apply_focus()` |
| `input.rs` | Input event dispatch, keybind matching with `FilterResult`, pointer motion/click handling |
| `workspace.rs` | `Workspace` struct with client list, focus tracking (focused_idx/last_focused_idx), focus_next/prev, zoom_master, layout toggle |
| `layout.rs` | `layout_workspace()` — tile (master-stack with mfact/nmaster/gaps) and monocle layouts |
| `actions.rs` | `Action` enum, `dispatch_action()` — spawn, kill, focus, workspace view/send, zoom, monocle, mfact adjust, quit |

### Data model

- `Vwm` owns 9 `Workspace`s, tracks `current_ws`/`previous_ws`
- Each `Workspace` has a `Vec<Window>`, `focused_idx`, `last_focused_idx`, layout state (kind, mfact, nmaster, gaps)
- `Space<Window>` is smithay's render-level abstraction — windows are mapped/unmapped here for compositing
- The workspace model drives layout; `apply_layout()` computes geometry and maps windows in the Space

### Event flow

1. `winit_evt.dispatch_new_events()` — pump winit events (resize, close, input)
2. Input events → `handle_input_event()` → keyboard filter → keybind match or forward to client
3. `apply_layout()` — compute tile/monocle geometry, configure toplevels, map in Space
4. `render_output()` — smithay composites all mapped surfaces via GlesRenderer
5. `display.dispatch_clients()` — process Wayland protocol messages

### Keybind system

Currently hardcoded in `input.rs::default_keybinds()`. Will be config-driven (M3).

Keybind matching: in the keyboard `input()` filter closure, compare the modified keysym + modifier state against the keybind list. `FilterResult::Intercept(action)` consumes the key, `FilterResult::Forward` sends it to the focused client.

## Key Design Decisions

- **smithay `Space<Window>`**: Used for rendering and surface-under queries. The workspace model (`Vec<Window>` per workspace) is separate — `apply_layout()` bridges the two by mapping/unmapping windows in the Space.
- **Focus fallback**: `last_focused_idx` in Workspace, same pattern as vwm's C implementation. When a window is removed, try last_focused first, then neighbor.
- **Tile layout**: Direct port of vwm's `layout_tile_in_area` — master-stack with mfact, nmaster, outer gaps, inner gaps between master/stack.
- **Config format**: Will use vwm's existing custom block-based format (improved with `$variables` and `{1-9}` range expansion). Not TOML/KDL/YAML — decided after comparing all options.
- **Rendering**: smithay GlesRenderer for compositing. Bar will use tiny-skia + cosmic-text (M4).

## Milestone Status

- **M1**: Skeleton — done (smithay winit backend, Wayland socket, xdg-shell, render loop)
- **M2**: Tiling — done (input, layout, focus, keybinds, actions, workspaces)
- **M3**: Config — pending (custom format parser in Rust, hot reload)
- **M4**: Bar — pending (tiny-skia rendering, modules)
- **M5**: Full features — pending (scratchpad, multi-monitor, rules, autostart)
- **M6**: Animations — pending (three-tier system: none/light/heavy)
- **M7**: Production — pending (udev/DRM, XWayland, session management)

## Picking Up Where We Left Off

**Last session**: 2026-04-04. M1+M2 built but NOT runtime-tested yet. Charlton prefers building solid before testing.

**Next task is M3 — config parser**:
1. Port vwm's C parser (`~/repos/vwm/src/config.c`, ~1900 lines) to Rust in `crates/vwm-config/`
2. The C parser uses `split_line_tokens()` for tokenization, a block state machine for nesting, and `config_unquote_inplace()` for quoted strings
3. Add `$variable` expansion (preprocessing pass, like Hyprland/Sway)
4. Add `{1-9}` range expansion in the binds block (expands to 9 keybinds)
5. All blocks: general, theme, animations, bar (with nested modules), scratchpad (with define), rules, autostart, commands, binds
6. Include files: `include "path"` with `~` expansion, max depth 16
7. Hot reload: SIGUSR1 or SIGHUP via `signal-hook` crate
8. Wire config into `Vwm` state — replace `default_keybinds()` with config-loaded keybinds
9. Reference config: `~/repos/vwm/example/vwm.conf` is the canonical example
10. Full plan: `~/.claude/plans/jolly-gathering-hartmanis.md`

**After M3**: M4 (bar), M5 (scratchpad/multi-monitor/rules), M6 (animations), M7 (udev/DRM/XWayland)

**Broader roadmap** (Charlton's notes): theming integration with loom-rs, notification daemon, sigil Wayland migration, rofi replacement

## Dependencies

- `smithay 0.7` — compositor toolkit (Wayland protocols, input, rendering)
- `tracing` / `tracing-subscriber` — structured logging

## Code Conventions

- Module-per-concern (input, layout, workspace, actions, state)
- `Vwm` is the central state struct — most methods are `impl Vwm` in the relevant module file
- smithay handlers (CompositorHandler, XdgShellHandler, etc.) live in `state.rs`
- Actions and input handling are in their own modules to keep state.rs focused on protocol handling

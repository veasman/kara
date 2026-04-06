# CLAUDE.md

This file provides guidance to Claude Code when working with code in this repository.

## Project

**kara** is a Wayland desktop environment built in Rust. Single monorepo, pure Wayland, all tools first-party. The sole developer is Charlton / veasman. Target platform: Artix Linux (Arch-based).

### Components

| Name | Role | Crate type | Status |
|------|------|-----------|--------|
| **kara-gate** | Compositor | binary | M1-M4 complete |
| **kara-sight** | Status bar | library (in compositor) | M4 complete |
| **kara-config** | Config parser | library | M3 complete |
| **kara-ipc** | IPC protocol | library | stub |
| **kara-color** | Color math | library | stub |
| **kara-theme** | Theme specs + renderers | library | stub |
| **kara-ui** | Shared rendering | library | stub |
| **kara-beautify** | Appearance manager (theme, wallpaper, animations, decorations, notification styling) | binary | stub |
| **kara-summon** | Application launcher | binary | stub |
| **kara-whisper** | Notification daemon (freedesktop D-Bus) | binary | stub |
| **kara-glimpse** | Screenshot tool | binary | stub |

## Build Commands

```bash
cargo build              # Build all crates (debug)
cargo build --release    # Build all crates (release)
cargo check              # Type-check without building
cargo run -p kara-gate   # Run compositor with winit backend
cargo test               # Run all tests (19 passing)
make run                 # Shortcut for cargo run -p kara-gate
```

## Architecture

**Compositor state**: `Gate` struct in `state.rs` holds all smithay protocol state, the workspace model, keybinds, and the smithay `Space<Window>`.

**Backend**: Currently winit-only (development). Production backend (udev/DRM + libinput) planned for M7.

### Key modules (`crates/kara-gate/src/`)

| Module | Role |
|--------|------|
| `main.rs` | Entry point, event loop, winit backend, render loop |
| `state.rs` | `Gate` struct, smithay handler impls, `apply_layout()`, `apply_focus()` |
| `input.rs` | Input event dispatch, keybind matching |
| `workspace.rs` | `Workspace` struct, focus tracking |
| `layout.rs` | Tile and monocle layouts |
| `actions.rs` | `Action` enum, `dispatch_action()` |

### Crate dependency graph

```
kara-color      (standalone)
kara-config     (standalone)
kara-ipc        (serde, serde_json)
kara-theme      -> kara-color
kara-ui         -> tiny-skia, cosmic-text
kara-sight      -> kara-config
kara-gate       -> kara-config, kara-sight, smithay
kara-beautify   -> kara-color, kara-theme, kara-ipc
kara-summon     -> kara-ipc, kara-ui
kara-whisper    -> kara-ipc, kara-ui
kara-glimpse    -> kara-ipc
```

### IPC (planned)

- Unix socket at `$XDG_RUNTIME_DIR/kara.sock`
- Length-prefixed JSON messages
- kara-gate = server, tools = clients

## Key Design Decisions

- **Pure Wayland**: No XWayland. No X11.
- **Separate processes**: kara-summon, kara-whisper, kara-glimpse are standalone Wayland clients using wlr-layer-shell.
- **kara-whisper uses D-Bus**: Implements `org.freedesktop.Notifications`.
- **kara-beautify**: Unified appearance manager — owns theme, wallpaper, animations, window decorations, notification styling. Much broader than a theme engine.
- **kara-sight stays in-compositor**: Bar needs frame-level rendering.
- **Config format**: Custom block-based with `$variables` and `{1-9}` range expansion.
- **Config path**: `~/.config/kara/kara-gate.conf`
- **State path**: `~/.local/state/kara/`

## Milestone Status

- **M1**: Skeleton — done
- **M2**: Tiling — done
- **M3**: Config — done (19 tests passing)
- **M4**: Bar — done
- **M5**: Single-monitor daily driver — done (environment, autostart, rules, floating, fullscreen, borders, cursor, scratchpad)
- **M6**: Multi-monitor — done (per-output workspaces, hybrid sync, monitor actions, animations)
- **M7**: Production backend — done (udev/DRM, libinput, libseat, xdg-decoration)
- **M8**: First-party tools — pending (kara-summon, kara-whisper, kara-glimpse)

## Picking Up Where We Left Off

**Last session**: 2026-04-05.

**What was done this session**:
1. `exec` keybind action, `toggle_float` docs, scratchpad workarea centering
2. Software cursor rendering (xcursor theme, auto-hide 1s/5px jitter)
3. `mod+Shift+{1-9}` fix (raw_syms), config auto-reload (mtime polling)
4. Border radius / rounded corners, key repeat tuning (500ms/33ms)
5. Monitor + sync bar modules merged, CPU bar module added
6. Clock icon fix, sync icon fix
7. Animation system: instant/clean/swoosh presets, position-only transforms
8. Per-CRTC vblank rendering, event-first loop ordering (input latency fix)
9. Performance: wallpaper/border/bar texture caching, Arc keybinds, output bounds cache
10. Named scratchpads as independent floating workspaces with own tiling
11. Scratchpad action routing (focus/zoom/monocle/mfact route to scratchpad workspace)
12. Dim overlay for visible scratchpads

**Next priorities**:
1. M8: First-party tools (kara-summon, kara-whisper, kara-glimpse)
2. GL shader support for opacity/scale animations (kara-beautify)
3. Input device config wiring to libinput
4. Scratchpad blur effect (needs GL shaders)

**Repo**: git@github.com:veasman/kara.git (local dir may be ~/repos/kara/)

**Full plan**: `~/.claude/plans/twinkly-prancing-moon.md`

## Dependencies

- `smithay 0.7` — compositor toolkit
- `tracing` / `tracing-subscriber` — structured logging
- `xkbcommon 0.8` — keysym name resolution
- `dirs 6` — XDG directory resolution
- `signal-hook 0.3` — SIGUSR1/SIGHUP for hot reload
- `tiny-skia 0.12` — 2D rendering
- `cosmic-text 0.18` — font shaping and glyph rendering
- `chrono 0.4` — clock module
- `serde` / `serde_json` — IPC serialization
- `xcursor 0.3` — cursor theme loading

## Code Conventions

- Module-per-concern (input, layout, workspace, actions, state)
- `Gate` is the central compositor state struct
- smithay handlers live in `state.rs`
- Actions and input in their own modules

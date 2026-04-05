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
- **M6**: Multi-monitor — done (per-output workspaces, hybrid sync, monitor actions). Animations pending.
- **M7**: Production backend — done (udev/DRM, libinput, libseat, xdg-decoration)
- **M8**: First-party tools — pending (kara-summon, kara-whisper, kara-glimpse)

## Picking Up Where We Left Off

**Last session**: 2026-04-05. M7 production backend + M6 multi-monitor.

**What was done this session**:
1. Fixed winit deadlock, bar centering, font family wiring, all warnings
2. Cleaned up vwm→kara naming, updated example config/README
3. M7: Production udev/DRM backend (libseat, libinput, GBM, DrmCompositor)
4. Backend selection via KARA_BACKEND env var (auto-detects winit vs udev)
5. xdg-decoration protocol (ServerSide — no client title bars)
6. M6: Multi-monitor with OutputState per-output model
7. Hybrid workspace model (sync/independent like vwm, mod+s toggle)
8. Monitor focus/send actions (mod+h/l, mod+Shift+h/l)
9. Per-output rendering with local coordinate spaces

**Runtime tested**: Both winit (nested X11) and udev (TTY). Dual monitor tested (laptop + TV).

**Known issue**: Multi-monitor coordinate space may need further testing — borders/windows showing correctly on each output after last fix (output-local coords) but not yet verified.

**Next priorities**:
1. Test dual-monitor thoroughly (workspace switching, sync toggle, window placement)
2. Animations (window open/close, workspace switch, scratchpad, focus transitions)
3. M8: First-party tools (kara-summon, kara-whisper, kara-glimpse)

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

## Code Conventions

- Module-per-concern (input, layout, workspace, actions, state)
- `Gate` is the central compositor state struct
- smithay handlers live in `state.rs`
- Actions and input in their own modules

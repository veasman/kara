# kara

A Wayland desktop environment. Compositor, status bar, theme engine, launcher, notifications, screenshots — all first-party, pure Wayland.

Written in Rust. Built for Arch/Artix Linux. MIT license.

## Components

| Name | Role | Status |
|------|------|--------|
| **kara-gate** | Wayland compositor | M1-M4 complete |
| **kara-sight** | Status bar (in compositor) | M4 complete |
| **kara-config** | Config parser | M3 complete |
| **kara-beautify** | Appearance manager | Planned |
| **kara-summon** | Application launcher | Planned |
| **kara-whisper** | Notification daemon | Planned |
| **kara-glimpse** | Screenshot tool | Planned |

## Build

```sh
make              # Release build
make debug        # Debug build
make check        # Type-check
make test         # Run tests
make run          # Run compositor (winit dev backend)
```

## Install

```sh
make install                        # Install to /usr/local
make PREFIX=/usr install            # Install to /usr
make install-config                 # Copy example config to ~/.config/kara/
```

## Configuration

Config file: `~/.config/kara/kara-gate.conf`

```sh
make install-config     # Copy example config
make reload             # Reload without restarting
```

Block-based format with `$variables`, `include`, and `{1-9}` range expansion:

```
$terminal = kitty

general {
    border_px 2
    gap_px 8
    default_mfact 0.5
}

theme {
    bg 0x111111
    accent 0x6bacac
}

commands {
    terminal "$terminal"
}

binds {
    "mod+Return" spawn "terminal"
    "mod+{1-9}" view_ws {1-9}
    "mod+Shift+{1-9}" send_ws {1-9}
}
```

## Project structure

```
kara/
  crates/
    kara-gate/         # Compositor
    kara-sight/        # Status bar
    kara-config/       # Config parser
    kara-ipc/          # IPC protocol
    kara-color/        # Color math
    kara-theme/        # Theme specs
    kara-ui/           # Shared rendering
    kara-beautify/     # Appearance manager
    kara-summon/       # Launcher
    kara-whisper/      # Notifications
    kara-glimpse/      # Screenshots
```

## License

[MIT](LICENSE)

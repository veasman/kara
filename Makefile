PREFIX    ?= /usr/local
BINDIR    ?= $(PREFIX)/bin
DATADIR   ?= $(PREFIX)/share
APPDIR    ?= $(DATADIR)/kara

TARGET    := kara-gate
CARGO     ?= cargo
PROFILE   ?= release

.PHONY: all build release debug check clean install install-config uninstall \
        pkg srcinfo reload run deps-check fmt clippy test

all: build

build: release

release:
	$(CARGO) build --release

debug:
	$(CARGO) build

check:
	$(CARGO) check

test:
	$(CARGO) test

fmt:
	$(CARGO) fmt --all

clippy:
	$(CARGO) clippy --all-targets

run:
	$(CARGO) run -p kara-gate

deps-check:
	@command -v cargo   >/dev/null 2>&1 || { echo "error: cargo not found — install rustup"; exit 1; }
	@command -v pkg-config >/dev/null 2>&1 || { echo "error: pkg-config not found"; exit 1; }
	@pkg-config --exists xkbcommon || { \
		echo "Missing runtime dependency: libxkbcommon"; \
		echo "On Arch/Artix:  doas pacman -S libxkbcommon"; \
		exit 1; \
	}
	@echo "build dependencies OK"

install:
	@test -f "target/release/$(TARGET)" || { echo "error: run 'make' first to build"; exit 1; }
	install -d "$(DESTDIR)$(BINDIR)"
	install -m 0755 "target/release/$(TARGET)" "$(DESTDIR)$(BINDIR)/$(TARGET)"
	install -m 0755 session/kara "$(DESTDIR)$(BINDIR)/kara"
	@test -f "target/release/kara-summon" && \
		install -m 0755 "target/release/kara-summon" "$(DESTDIR)$(BINDIR)/kara-summon" || true
	@test -f "target/release/kara-glimpse" && \
		install -m 0755 "target/release/kara-glimpse" "$(DESTDIR)$(BINDIR)/kara-glimpse" || true
	@test -f "target/release/kara-whisper" && \
		install -m 0755 "target/release/kara-whisper" "$(DESTDIR)$(BINDIR)/kara-whisper" || true
	@test -f "target/release/kara-beautify" && \
		install -m 0755 "target/release/kara-beautify" "$(DESTDIR)$(BINDIR)/kara-beautify" || true
	install -d "$(DESTDIR)$(APPDIR)"
	install -m 0644 example/kara-gate.conf "$(DESTDIR)$(APPDIR)/kara-gate.conf.example"
	install -m 0644 example/kara-beautify.toml "$(DESTDIR)$(APPDIR)/kara-beautify.toml.example"
	install -d "$(DESTDIR)$(DATADIR)/wayland-sessions"
	install -m 0644 session/kara.desktop "$(DESTDIR)$(DATADIR)/wayland-sessions/kara.desktop"
	install -d "$(DESTDIR)$(DATADIR)/xdg-desktop-portal"
	install -m 0644 session/kara-portals.conf "$(DESTDIR)$(DATADIR)/xdg-desktop-portal/kara-portals.conf"
	# Ship every theme's manifest (theme.toml) to $APPDIR/themes/.
	# We purge the existing themes subdir first so removed-from-repo
	# themes disappear on upgrade instead of lingering as stale
	# entries in the theme picker.
	#
	# WALLPAPERS ARE NOT INSTALLED BY THIS TARGET. Binary blobs are
	# excluded from the git repo via .gitignore and shipped through
	# a separate channel (future: `kara-beautify theme fetch`). For
	# now users drop wallpapers into ~/.local/share/kara/themes/<name>/
	# wallpapers/ by hand — kara-beautify's theme search path prefers
	# $XDG_DATA_HOME over the system install, so per-user wallpapers
	# Just Work alongside a system-installed manifest.
	rm -rf "$(DESTDIR)$(APPDIR)/themes"
	@for theme in themes/*/; do \
		name="$$(basename $$theme)"; \
		[ -f "$$theme/theme.toml" ] || continue; \
		install -d "$(DESTDIR)$(APPDIR)/themes/$$name"; \
		install -m 0644 "$$theme/theme.toml" "$(DESTDIR)$(APPDIR)/themes/$$name/theme.toml"; \
	done
	install -d "$(DESTDIR)$(DATADIR)/licenses/kara"
	install -m 0644 LICENSE "$(DESTDIR)$(DATADIR)/licenses/kara/LICENSE"
	@mkdir -p "$(HOME)/.config/kara"
	@if [ -f "$(HOME)/.config/kara/kara-gate.conf" ]; then \
		echo "Config exists: ~/.config/kara/kara-gate.conf (not overwriting)"; \
	else \
		install -m 0644 example/kara-gate.conf "$(HOME)/.config/kara/kara-gate.conf"; \
		echo "Installed config: ~/.config/kara/kara-gate.conf"; \
	fi

uninstall:
	rm -f "$(DESTDIR)$(BINDIR)/$(TARGET)"
	rm -f "$(DESTDIR)$(BINDIR)/kara"
	rm -f "$(DESTDIR)$(BINDIR)/kara-summon"
	rm -f "$(DESTDIR)$(BINDIR)/kara-glimpse"
	rm -f "$(DESTDIR)$(BINDIR)/kara-whisper"
	rm -f "$(DESTDIR)$(BINDIR)/kara-beautify"
	rm -f "$(DESTDIR)$(APPDIR)/kara-gate.conf.example"
	rm -f "$(DESTDIR)$(APPDIR)/kara-beautify.toml.example"
	rm -f "$(DESTDIR)$(DATADIR)/wayland-sessions/kara.desktop"
	rm -f "$(DESTDIR)$(DATADIR)/xdg-desktop-portal/kara-portals.conf"
	rm -f "$(DESTDIR)$(DATADIR)/licenses/kara/LICENSE"

pkg:
	makepkg -fs

srcinfo:
	makepkg --printsrcinfo > .SRCINFO

reload:
	pkill -USR1 -x $(TARGET) || true

clean:
	$(CARGO) clean

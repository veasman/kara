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

install: release
	install -d "$(DESTDIR)$(BINDIR)"
	install -m 0755 "target/release/$(TARGET)" "$(DESTDIR)$(BINDIR)/$(TARGET)"
	install -d "$(DESTDIR)$(APPDIR)"
	install -m 0644 example/kara-gate.conf "$(DESTDIR)$(APPDIR)/kara-gate.conf.example"
	install -d "$(DESTDIR)$(DATADIR)/wayland-sessions"
	install -m 0644 session/kara.desktop "$(DESTDIR)$(DATADIR)/wayland-sessions/kara.desktop"
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
	rm -f "$(DESTDIR)$(APPDIR)/kara-gate.conf.example"
	rm -f "$(DESTDIR)$(DATADIR)/wayland-sessions/kara.desktop"
	rm -f "$(DESTDIR)$(DATADIR)/licenses/kara/LICENSE"

pkg:
	makepkg -fs

srcinfo:
	makepkg --printsrcinfo > .SRCINFO

reload:
	pkill -USR1 -x $(TARGET) || true

clean:
	$(CARGO) clean

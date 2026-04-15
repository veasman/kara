pkgname=kara-git
pkgver=r0.0000000
pkgrel=1
pkgdesc="kara desktop environment"
arch=('x86_64')
url="https://github.com/veasman/kara"
license=('MIT')
depends=(
	'libxkbcommon'
	'wayland'
	'mesa'
	'libinput'
	'seatd'
	'fontconfig'
	# kara wraps the session in `dbus-run-session` via /usr/bin/kara
	# so a user session D-Bus is always present even on OpenRC/runit.
	'dbus'
	# kara ships /usr/share/xdg-desktop-portal/kara-portals.conf routing
	# the Settings interface through the GTK backend, which is what
	# delivers live color-scheme updates to Firefox / GTK apps on
	# kara-beautify theme switch. Without this the portal has no impl
	# for the kara desktop and live dark/light flip silently no-ops.
	'xdg-desktop-portal'
	'xdg-desktop-portal-gtk'
	# Video wallpaper decode via gstreamer appsink. kara-gate and
	# kara-summon (thumbnails) both link against gstreamer + the
	# app/video libraries. gst-plugins-{base,good,bad} + gst-libav
	# provide the runtime codec + demuxer plugins — without these
	# the pipeline fails to link at playback time and video
	# wallpapers silently fall back to a black frame.
	'gstreamer'
	'gst-plugins-base'
	'gst-plugins-good'
	'gst-plugins-bad'
	'gst-libav'
	# The bar's media module shells out to `playerctl status` and
	# `playerctl metadata ...` every status tick. Without it the module
	# silently stays blank, which has bitten the maintainer twice.
	# Promote to a hard dep instead of optdepends.
	'playerctl'
)
makedepends=(
	'base-devel'
	'git'
	'cargo'
	'pkgconf'
)
optdepends=(
	'wireplumber: volume module (wpctl)'
	'brightnessctl: brightness keybinds'
	'wl-clipboard: screenshot clipboard copy'
	'foot: default terminal'
	'bibata-cursor-theme: default cursor theme'
	'gruvbox-plus-icon-theme: example icon theme for the default theme'
	'nord-nvim: hand-tuned Nord palette for kara.nvim plugin dispatch'
	'gruvbox-nvim: hand-tuned Gruvbox palette for kara.nvim plugin dispatch'
)
provides=(
	'kara'
	'kara-gate'
	'kara-summon'
	'kara-glimpse'
	'kara-whisper'
	'kara-beautify'
)
conflicts=('kara' 'kara-gate')
source=("git+https://github.com/veasman/kara.git")
sha256sums=('SKIP')

pkgver() {
	cd "$srcdir/kara"
	printf "r%s.%s" \
		"$(git rev-list --count HEAD)" \
		"$(git rev-parse --short=7 HEAD)"
}

build() {
	cd "$srcdir/kara"
	export CARGO_TARGET_DIR=target
	cargo build --release --locked
}

package() {
	cd "$srcdir/kara"
	make DESTDIR="$pkgdir" PREFIX=/usr install
}

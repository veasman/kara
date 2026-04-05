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
)
makedepends=(
	'base-devel'
	'git'
	'cargo'
	'pkgconf'
)
optdepends=(
	'wireplumber: volume module (wpctl)'
	'playerctl: media module'
	'brightnessctl: brightness keybinds'
	'kitty: default terminal'
)
provides=('kara' 'kara-gate')
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

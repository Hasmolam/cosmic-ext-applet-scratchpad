default: build

NAME := 'cosmic-ext-applet-scratchpad'
APPID := 'io.github.hasmolam.cosmic-ext-applet-scratchpad'

cargo-target-dir := env('CARGO_TARGET_DIR', 'target')
bin-src := cargo-target-dir / 'release' / NAME

rootdir := ''
prefix := '/usr'

base-dir := absolute_path(clean(rootdir / prefix))
share-dst := base-dir / 'share'

bin-dst := base-dir / 'bin' / NAME
desktop-dst := share-dst / 'applications' / APPID + '.desktop'
metainfo-dst := share-dst / 'metainfo' / APPID + '.metainfo.xml'
icon-dst := share-dst / 'icons/hicolor/scalable/apps' / APPID + '-symbolic.svg'

build *args:
	cargo build --release {{args}}

run:
	cargo run

check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

install: build
	install -Dm0755 {{ bin-src }} {{ bin-dst }}
	install -Dm0644 data/icons/scalable/apps/{{ APPID }}-symbolic.svg {{ icon-dst }}
	install -Dm0644 data/{{ APPID }}.desktop {{ desktop-dst }}
	install -Dm0644 data/{{ APPID }}.metainfo.xml {{ metainfo-dst }}

install-user:
	just prefix={{ env_var('HOME') }}/.local install

uninstall:
	rm -f {{ bin-dst }}
	rm -f {{ icon-dst }}
	rm -f {{ desktop-dst }}
	rm -f {{ metainfo-dst }}

uninstall-user:
	just prefix={{ env_var('HOME') }}/.local uninstall

# Vendor Cargo dependencies for offline builds
vendor:
	mkdir -p .cargo
	cargo vendor --locked | head -n -1 > .cargo/config.toml
	echo 'directory = "vendor"' >> .cargo/config.toml
	tar pcf vendor.tar vendor
	rm -rf vendor

# Extract vendored Cargo dependencies
vendor-extract:
	rm -rf vendor
	tar pxf vendor.tar

# Build release binary using vendored dependencies
build-vendored *args: vendor-extract (build '--frozen --offline' args)

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
icon-dst := share-dst / 'icons/hicolor/scalable/apps' / APPID + '-symbolic.svg'

build:
	cargo build --release

run:
	cargo run

check:
	cargo fmt --check
	cargo clippy -- -D warnings
	cargo test

install: build
	install -Dm0755 {{ bin-src }} {{ bin-dst }}
	install -Dm0644 data/icons/scalable/apps/{{ APPID }}-symbolic.svg {{ icon-dst }}
	install -Dm0644 data/{{ APPID }}.desktop {{ desktop-dst }}

install-user:
	just prefix={{ env_var('HOME') }}/.local install

uninstall:
	rm -f {{ bin-dst }}
	rm -f {{ icon-dst }}
	rm -f {{ desktop-dst }}

uninstall-user:
	just prefix={{ env_var('HOME') }}/.local uninstall

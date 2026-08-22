run:
	cargo run -p zed

build:
	cargo build -p zed

bufrpc-run:
	ZED_BUFFER_RPC_SOCK="${ZED_BUFFER_RPC_SOCK:-/tmp/zed-bufrpc.sock}" cargo run -p zed

install:
	#!/usr/bin/env bash
	set -uo pipefail
	if [[ "$(uname -s)" != "Darwin" ]]; then
		echo "install is only supported on macOS" >&2
		exit 1
	fi
	# bundle-mac -i builds a release bundle and moves it into /Applications, then
	# (release flow) tries to build a DMG from the now-moved app and fails. That
	# post-install DMG step is irrelevant to a local install, so we tolerate its
	# failure and assert the app actually landed in /Applications instead.
	app="/Applications/Zed RPC.app"
	rm -rf "$app"
	script/bundle-mac -i || true
	if [[ -d "$app" ]]; then
		echo "Installed: $app"
	else
		echo "install failed: $app was not created" >&2
		exit 1
	fi

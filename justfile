run:
	cargo run -p zed

build:
	cargo build -p zed

bufrpc-run:
	ZED_BUFFER_RPC_SOCK="${ZED_BUFFER_RPC_SOCK:-/tmp/zed-bufrpc.sock}" cargo run -p zed

install:
	#!/usr/bin/env bash
	set -euo pipefail
	if [[ "$(uname -s)" != "Darwin" ]]; then
		echo "install is only supported on macOS" >&2
		exit 1
	fi
	script/bundle-mac -i

#!/bin/sh
set -eu
cd "$(dirname "$0")"
exec rustup run stable cargo run -- "$@"

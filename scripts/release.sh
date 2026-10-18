#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
home=${HOME:-}
cargo_home=${CARGO_HOME:-$home/.cargo}
sep=$(printf '\037')
flags="--remap-path-prefix=$home=~${sep}--remap-path-prefix=$cargo_home=/cargo${sep}--remap-path-prefix=$root=/zarn"

cd "$root"
CARGO_ENCODED_RUSTFLAGS="$flags" CARGO_TARGET_DIR="$root/target/dist" cargo build --release -p zarn --bin zarn "$@"
binary="$root/target/dist/release/zarn"
[ -f "$binary.exe" ] && binary="$binary.exe"
echo "built $binary"

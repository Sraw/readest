#!/bin/sh
# Builds ebk.wasm and puts it where Readest loads it from. Needs the Rust target wasm32-unknown-unknown
# (rustup target add wasm32-unknown-unknown). The built file is committed, so building Readest does not need this.
set -e
here=$(cd "$(dirname "$0")" && pwd)
cargo build --quiet --release -p ebk-wasm --target wasm32-unknown-unknown --manifest-path "$here/Cargo.toml"
cp "$here/target/wasm32-unknown-unknown/release/ebk_wasm.wasm" "$here/../../apps/readest-app/src/libs/ebk/ebk.wasm"

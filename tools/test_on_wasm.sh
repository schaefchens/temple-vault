#!/bin/sh
# Runs Temple's tests on wasm32 in Node, as the web runs Temple: 32-bit, one thread, the
# browser's random source and clock. Arguments go to cargo test. Needs rustup's
# wasm32-unknown-unknown target, wasm-bindgen-cli at Cargo.lock's wasm-bindgen version, Node,
# and a clang with a wasm32 backend for SQLite – Apple's has none, Homebrew's llvm does.
set -e

export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/.."

llvm=/opt/homebrew/opt/llvm/bin
if [ -z "$CC_wasm32_unknown_unknown" ] && [ -x "$llvm/clang" ]; then
  export CC_wasm32_unknown_unknown="$llvm/clang" AR_wasm32_unknown_unknown="$llvm/llvm-ar"
fi
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner
cargo test --release --target wasm32-unknown-unknown --lib "$@"

#!/bin/sh
# Builds the wasm module and its TypeScript into src/generated. SQLite's C code needs a clang
# with a wasm32 backend – Apple's has none, Homebrew's llvm does – and ubrn needs
# wasm-bindgen-cli at Cargo.lock's wasm-bindgen version.
set -e

export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")"

llvm=/opt/homebrew/opt/llvm/bin
if [ -z "$CC_wasm32_unknown_unknown" ] && [ -x "$llvm/clang" ]; then
  export CC_wasm32_unknown_unknown="$llvm/clang" AR_wasm32_unknown_unknown="$llvm/llvm-ar"
fi
npx ubrn build wasm2 --config ubrn.config.yaml --release

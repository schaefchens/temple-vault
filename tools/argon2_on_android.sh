#!/bin/sh
# Times Argon2 for the phrase key on the Android device adb sees – a phone over USB, or the
# one ANDROID_SERIAL names: examples/argon2_time.rs. Needs ANDROID_HOME, cargo-ndk and
# rustup's aarch64-linux-android target; the device must run 64-bit ARM.
set -e

# rustup's toolchain first: a Homebrew Rust earlier in PATH has no Android targets.
export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/.."
cargo ndk -t arm64-v8a -P 26 build --release --example argon2_time
adb push target/aarch64-linux-android/release/examples/argon2_time /data/local/tmp/ >/dev/null
adb shell getprop ro.product.model
adb shell /data/local/tmp/argon2_time

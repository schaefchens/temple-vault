#!/bin/sh
# Runs Temple's tests on the Android device adb sees – an emulator or a phone; ANDROID_SERIAL
# picks one – and on the booted iOS simulator. Needs ANDROID_HOME, cargo-ndk and rustup's
# aarch64-linux-android and aarch64-apple-ios-sim targets.
set -e

# rustup's toolchain first: a Homebrew Rust earlier in PATH has no Android or iOS targets.
export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/.."

cargo ndk -t arm64-v8a -P 26 test --release --no-run
tests=$(ls -t target/aarch64-linux-android/release/deps/temple_vault-* | grep -v '\.d$' | head -1)
adb push "$tests" /data/local/tmp/temple-tests >/dev/null
adb shell "cd /data/local/tmp && TMPDIR=/data/local/tmp ./temple-tests"

cargo test --release --target aarch64-apple-ios-sim --no-run
tests=$(ls -t target/aarch64-apple-ios-sim/release/deps/temple_vault-* | grep -v '\.d$' | head -1)
xcrun simctl spawn booted "$PWD/$tests"

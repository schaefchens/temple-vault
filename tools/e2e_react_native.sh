#!/bin/sh
# Builds the React Native demo for release and runs its Maestro flows (examples/react-native/
# e2e): on the iOS simulator IOS_SIMULATOR names – a name or UDID, by default the first
# booted one – and on the Android device ANDROID_SERIAL names, by default the first adb sees.
# `tools/e2e_react_native.sh ios` or `… android` runs one platform. Needs Maestro, Xcode with
# CocoaPods, the Android SDK, and the bindings built: npm run build:ios and build:android in
# bindings/react-native.
set -e

cd "$(dirname "$0")/../examples/react-native"
export EXPO_NO_TELEMETRY=1 LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
adb="$ANDROID_HOME/platform-tools/adb"
# Gradle and Maestro need a JDK; macOS's own `java` is only a stub.
jdk=/opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home
if [ -z "$JAVA_HOME" ] && [ -d "$jdk" ]; then
  export JAVA_HOME="$jdk"
fi

for platform in ${*:-ios android}; do
  case $platform in
    ios)
      simulator=${IOS_SIMULATOR:-$(xcrun simctl list devices booted | grep -m1 -oE '[0-9A-F-]{36}')}
      npx expo run:ios --configuration Release --no-bundler --device "$simulator"
      udid=$(xcrun simctl list devices | grep -F "$simulator" | grep -m1 -oE '[0-9A-F-]{36}')
      maestro --device "$udid" test e2e
      ;;
    android)
      serial=${ANDROID_SERIAL:-$("$adb" devices | awk 'NR == 2 { print $1 }')}
      # Expo picks a device by name: an emulator's is its AVD's.
      name=$("$adb" -s "$serial" emu avd name 2>/dev/null | head -1 | tr -d '\r')
      npx expo run:android --variant release --no-bundler --device "${name:-$serial}"
      maestro --device "$serial" test e2e
      ;;
    *)
      echo "unknown platform: $platform (ios or android)" >&2
      exit 1
      ;;
  esac
done

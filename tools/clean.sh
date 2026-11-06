#!/bin/sh
# Frees disk space: removes what builds and tests left behind – all of it made again by the build
# scripts and ubrn. Only paths git ignores are touched in the repository; outside it, only Xcode's
# build folder for the example's workspace, and – when asked – Temple's own test devices.
#
#   sh tools/clean.sh               builds and test leftovers
#   sh tools/clean.sh --all         also node_modules
#   sh tools/clean.sh --devices     also Temple's simulators ("Temple …") and emulators (temple_…),
#                                   shut down first; other projects' devices stay
#   sh tools/clean.sh -n [...]      lists what would go, with sizes, and removes nothing
#
# Kept always: what the fuzzer found – its corpus and the inputs that broke something
# (fuzz/corpus, fuzz/artifacts).
set -eu
cd "$(dirname "$0")/.."
root=$(pwd)

dry=0 deps=0 devices=0
for arg in "$@"; do
  case $arg in
    -n | --dry-run) dry=1 ;;
    --all) deps=1 ;;
    --devices) devices=1 ;;
    *) echo "usage: sh tools/clean.sh [-n] [--all] [--devices]" >&2; exit 1 ;;
  esac
done

# Builds: the Rust workspace, the FFI crate's, the fuzzer's and the spikes', what ubrn generates
# for React Native and the web, the packages' builds, and the React Native example's native
# projects, which Expo's prebuild makes.
builds='target bindings/ffi/target fuzz/target spikes/*/target
bindings/*/dist bindings/web/src/generated
bindings/react-native/src/generated bindings/react-native/src/index.tsx
bindings/react-native/ios bindings/react-native/android bindings/react-native/TempleReactNative.podspec
examples/react-native/ios examples/react-native/android examples/react-native/.expo examples/web/dist'

leftovers='examples/web/test-results examples/web/playwright-report fuzz/coverage'

modules='node_modules bindings/*/node_modules examples/*/node_modules'

# An emulator running the AVD named: told to stop, and waited for.
stop_emulator() {
  command -v adb > /dev/null || return 0
  for serial in $(adb devices 2> /dev/null | awk '/^emulator-/ { print $1 }'); do
    [ "$(adb -s "$serial" emu avd name 2> /dev/null | head -1 | tr -d '\r')" = "$1" ] || continue
    adb -s "$serial" emu kill > /dev/null 2>&1
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      adb devices | grep -q "^$serial" || break
      sleep 2
    done
  done
}

patterns="$builds $leftovers"
[ $deps = 1 ] && patterns="$patterns $modules"

total=0
remove() { # path, shown as, and how to remove it if not by deleting the path
  kb=$(du -sk "$1" 2> /dev/null | cut -f1)
  total=$((total + ${kb:-0}))
  printf '%9.1f MB  %s\n' "$(echo "${kb:-0} / 1024" | bc -l)" "$2"
  [ $dry = 1 ] && return
  if [ $# -gt 2 ]; then eval "$3"; else rm -rf "$1"; fi
}

for pattern in $patterns; do
  for path in $pattern; do
    [ -e "$path" ] || continue
    # Never what git keeps, nor what it would: only what it ignores.
    if ! git check-ignore -q "$path"; then
      echo "kept, not ignored by git: $path" >&2
      continue
    fi
    remove "$path" "$path"
  done
done

# Xcode's build folder for the example – the one whose workspace is in this repository.
if [ "$(uname)" = Darwin ]; then
  for folder in "$HOME"/Library/Developer/Xcode/DerivedData/*/; do
    [ -f "$folder/info.plist" ] || continue
    workspace=$(/usr/libexec/PlistBuddy -c 'Print :WorkspacePath' "$folder/info.plist" 2> /dev/null || true)
    case $workspace in "$root"/*) remove "${folder%/}" "${folder%/} (Xcode, ${workspace#"$root"/})" ;; esac
  done
fi

# Temple's own test devices, by their names: simulators "Temple …", Android emulators temple_….
if [ $devices = 1 ]; then
  newline='
'
  if [ "$(uname)" = Darwin ] && command -v xcrun > /dev/null; then
    simulators=$(xcrun simctl list devices -j | node -e "
      let json = '';
      process.stdin.on('data', (d) => (json += d)).on('end', () => {
        for (const list of Object.values(JSON.parse(json).devices))
          for (const d of list) if (d.name.startsWith('Temple ')) console.log(d.udid + ' ' + d.name);
      });")
    IFS=$newline
    for simulator in $simulators; do
      udid=${simulator%% *}
      remove "$HOME/Library/Developer/CoreSimulator/Devices/$udid" "simulator ${simulator#* } ($udid)" \
        "xcrun simctl shutdown $udid > /dev/null 2>&1; xcrun simctl delete $udid"
    done
    unset IFS
  fi
  avds=${ANDROID_AVD_HOME:-$HOME/.android/avd}
  for folder in "$avds"/temple_*.avd; do
    [ -d "$folder" ] || continue
    name=$(basename "$folder" .avd)
    remove "$folder" "emulator $name" "stop_emulator $name; rm -rf '$folder' '$avds/$name.ini'"
  done
fi

verb=$([ $dry = 1 ] && echo "would free" || echo "freed")
printf '%s %.1f GB\n' "$verb" "$(echo "$total / 1048576" | bc -l)"

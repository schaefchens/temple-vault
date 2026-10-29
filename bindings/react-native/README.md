# @schaefchens/temple-react-native

[Temple](https://github.com/schaefchens/temple-vault) for React Native on iOS and Android: encrypted storage with plausible deniability – a vault is one file holding any number of spaces, each opened by its own 12 words, with a SQLite database and large files in each. Made with AI and not yet audited: read the [project's README](https://github.com/schaefchens/temple-vault#readme) before your users' safety depends on it.

```sh
npm install @schaefchens/temple-react-native @schaefchens/temple @ubjs/react-native@0.31.0-6
cd ios && pod install    # with Expo: npx expo prebuild
```

`@ubjs/react-native` loads Temple's native library and must be a direct dependency of the app. For React Native's New Architecture, not Expo Go; the library is built for 64-bit phones, the iOS simulator and the Android emulator, Android from API level 24. How to use it: [Use](https://github.com/schaefchens/temple-vault#use); React hooks: `@schaefchens/temple-react`.

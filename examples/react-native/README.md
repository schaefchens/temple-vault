# Notes on Temple – React Native

An Expo app for iOS and Android: one vault in the app's documents, spaces opened by 12 words, notes with full-text search, files as objects, quick unlock behind the device's biometrics. Leaving the app locks the space; `temple-demo://self-test` runs Temple's self-test on the device.

```sh
npm install                          # in the repository root
npm run build:ios -w @schaefchens/temple-react-native       # or build:android
npx expo run:ios                     # or: npx expo run:android
```

E2E: `tools/e2e_react_native.sh` builds for release and runs the Maestro flows in `e2e/`.

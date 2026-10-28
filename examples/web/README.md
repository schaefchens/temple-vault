# Notes on Temple – web

The React Native demo as a PWA: one vault in the origin private file system, spaces opened by 12 words, notes with full-text search, files as objects. It installs and works offline; hiding the page locks the space; `?self-test` runs Temple's self-test in the browser.

```sh
npm install                                  # in the repository root
npm run build -w @schaefchens/temple-web     # the wasm module and its TypeScript
npm run dev -w temple-demo-web
```

E2E: `npm run e2e -w temple-demo-web` runs the Playwright specs in `e2e/` against the production build, in Chromium, Firefox and WebKit.

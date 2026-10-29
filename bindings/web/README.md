# @schaefchens/temple-web

[Temple](https://github.com/schaefchens/temple-vault) for the web: encrypted storage with plausible deniability – a vault is one file holding any number of spaces, each opened by its own 12 words, with a SQLite database and large files in each. Made with AI and not yet audited: read the [project's README](https://github.com/schaefchens/temple-vault#readme) before your users' safety depends on it.

```sh
npm install @schaefchens/temple-web @schaefchens/temple
```

Temple runs in a web worker of its own, as wasm, and keeps vaults in the origin private file system: serve the page over HTTPS or from localhost; WebKit's private sessions have no such file system. Vite takes the package as it is. How to use it: [Use](https://github.com/schaefchens/temple-vault#use); React hooks: `@schaefchens/temple-react`.

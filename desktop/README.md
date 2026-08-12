# Smiðr desktop shell

Electron owns one Smiðr backend process and loads its loopback URL in a secure
`BrowserWindow`. The backend is always started on an OS-selected port, never
opens an external browser, and is terminated when Electron exits.

The desktop runtime has a strict freshness contract:

- `prepare:production` rebuilds and checks the Svelte frontend.
- Rust is tested and built with `--features embed-frontend`.
- A content-derived build ID is compiled into Rust and written to the package
  manifest.
- Electron calls `/api/health` and refuses to show the window unless the
  backend reports an embedded frontend and the exact same build ID.
- The MCP server and Python package source travel with the app. A verified
  local CadQuery path is used when available; on a new Mac, Smiðr can create a
  persistent private environment under its Application Support directory.

## Commands

```bash
npm test                 # desktop lifecycle/build-contract unit tests
npm run prepare:production
npm run smoke:backend    # headless packaged-runtime verification
npm run start            # fresh production build, then Electron
npm run package:dir      # unpacked Linux application
npm run dist:linux       # AppImage
npm run install:linux    # AppImage + local launcher/menu entry
npm run package:mac      # signed/notarized macOS app when credentials exist
npm run package:mac:unsigned
npm run dist:mac         # signed/notarized DMG + ZIP
npm run dist:mac:unsigned
npm run install:mac      # local unsigned app in ~/Applications
```

## macOS

macOS packages must be built on macOS. `package:mac` and `dist:mac` build for
the machine's native architecture; Apple Silicon and Intel are both exercised
by `.github/workflows/macos-desktop.yml` on native GitHub runners. The package
step verifies the copied Rust backend through `/api/health` after it is inside
the `.app` bundle.

For local use:

```bash
brew install python@3.11
npm run install:mac
```

If the packaged build-machine Python no longer exists, the first launch looks
for `~/Library/Application Support/Smiðr/python/bin/python3`. When absent, it
offers to create that environment with Homebrew Python 3.11 and install the
bundled `ai3d-cad` package and its CadQuery dependencies.

For direct distribution, configure a Developer ID Application certificate via
electron-builder's `CSC_LINK` and `CSC_KEY_PASSWORD`, plus one supported set of
Apple notarization credentials, then run `npm run dist:mac`. With credentials
present, hardened runtime, signing verification, notarization, and stapling are
enabled. The `:unsigned` commands are only for native CI and local testing.

`npm run start:prepared` intentionally skips rebuilding and is only useful
while iterating on Electron window code against an already prepared runtime.
Normal usage and packaging should use the freshness-gated commands above.

# Rimaia

Cross-platform desktop app (macOS, Windows, Linux) built with [Tauri 2](https://tauri.app), React 19, TypeScript and Vite.

Current UI is a placeholder counter — increase, decrease, reset.

## Prerequisites

- Node.js 20+ and npm
- Rust stable (`rustup`)
- Platform toolchain per the [Tauri prerequisites](https://tauri.app/start/prerequisites/):
  - **macOS**: Xcode Command Line Tools
  - **Windows**: MSVC build tools + WebView2 runtime
  - **Linux**: `webkit2gtk-4.1`, `libappindicator3`, `librsvg2`, `patchelf`

## Development

```bash
npm install
npm run tauri dev
```

## Build

```bash
npm run tauri build
```

Bundles are written to `src-tauri/target/release/bundle/`. Tauri does not cross-compile — build each target on its own OS (or in CI).

## Layout

| Path                        | Purpose                              |
| --------------------------- | ------------------------------------ |
| `src/`                      | React frontend                       |
| `src-tauri/src/`            | Rust backend, entry point in `lib.rs` |
| `src-tauri/tauri.conf.json` | Window, bundle and app config        |
| `src-tauri/capabilities/`   | Permissions granted to the frontend  |

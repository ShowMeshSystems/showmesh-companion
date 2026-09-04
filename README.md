# ShowMesh Companion

ShowMesh Companion is a small, cross-platform desktop host for capabilities that must run on an endpoint beside show software or an LTC consumer. It keeps configuration in its own settings UI and advertises only the modules that are enabled and configured.

The current repository is a **foundation build**. It provides the desktop shell, persistent settings, capability derivation, Show Night arming policy, temporary overrides, and an honest status surface. Process observation/relaunch, audio capture/LTC decoding, coordinator transport, login registration, signing, and installers are not operational yet.

## Initial modules

- **Resolume supervision** will observe Resolume Arena locally and launch it when absent and armed. It will not expose kill, stop, or forced-restart controls.
- **LTC observation** will report timecode received by this computer. That evidence must remain distinct from any claim that Resolume consumed or followed the timecode.

Each configured module contributes a capability (`process.supervise` or `timecode.ltc.observe`). Future modules can use the same lifecycle without turning the application into a Resolume-specific daemon.

## Development

Requirements are Node.js, npm, Rust, and the platform prerequisites from the Tauri 2 documentation.

```sh
npm ci
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri dev
```

The macOS deployment target is 11.0 (Big Sur). Windows 10 remains the required Windows floor. Those runtime floors still need packaged-artifact verification on real machines before release.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for boundaries and the planned runtime flow.

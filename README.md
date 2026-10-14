# ShowMesh Companion

ShowMesh Companion is a small, cross-platform desktop application that runs on an endpoint machine beside show software. It watches for Resolume Arena and relaunches it when Resolume is absent and the application is armed, and it reports LTC timecode observed on that machine. ShowMesh remains authoritative for show state; the companion applies bounded local policy and reports what it observed locally.

## What is implemented, and what is verified

Every module described below is implemented and covered by unit tests. None of it has been exercised on real hardware. The build was written and tested with no Resolume install, no audio interface, no LTC source, and no GUI session, so no runtime behavior has been observed: not the tray, not the settings window, not a launch of Resolume, not a decoded frame from a real interface. Treat the whole application as unproven until it has been worked through on a bench.

`docs/BENCH-VERIFICATION.md` is the checklist that closes that gap. It is a release gate, not a suggestion.

## What the application does

- **Resolume supervision.** A poll thread looks for a process matching the configured Resolume Arena path every 2 seconds. If the process is absent and the application is armed, it launches Resolume, waits for it to appear, and backs off after a failed attempt. It never kills, stops, or forcibly restarts anything, and it never will: that boundary is a project constraint, not a missing feature.
- **LTC observation.** A capture thread opens the configured audio input, decodes LTC from the configured channel, and reports the observed frame, the detected frame rate, the input level, and how long ago the last frame decoded. This is evidence that this computer received timecode. It is never evidence that Resolume or any other consumer received or followed it.

Each configured module contributes a capability candidate (`process.supervise` or `timecode.ltc.observe`). Candidates are not advertised anywhere yet: the companion reads from the coordinator but does not report to it.

## Using it

The tray icon is the primary surface. The application has no dock icon on macOS (it runs under the `Accessory` activation policy) and hides rather than quits when the settings window is closed. The tray menu shows three live status lines (arming state, Resolume supervision, LTC observation) and offers:

- Open settings.
- Arm until the night ends, Disarm until the next night, Clear override.
- Quit ShowMesh Companion.

A temporary override is saved on this computer and survives a restart. It outranks the configured arming policy in both directions.

The settings window is where everything is configured. There is no configuration file to hand-edit.

- Companion name and coordinator address.
- Arming policy: Follow Show Night, Always armed, or Always disarmed.
- Resolume supervision: on or off, plus the path to the Resolume Arena application.
- LTC observation: on or off, plus the audio input device and the channel carrying LTC. The device list is refreshed every 2 seconds and can be refreshed on demand.
- A live timecode readout at 10 Hz, with lock state, detected frame rate, drop-frame flag, frame age, and input level.

Settings are validated when they are saved and again when they are loaded. A settings file that cannot be parsed, or that parses but holds unusable values, is kept aside as `settings.json.invalid`, and the application loads the previous saved generation if there is one, or defaults if there is not. The window says which happened.

`Open at login` is saved but does nothing yet: the application does not register itself with the operating system.

## Following Show Night

With a coordinator address set, the companion reads `GET /api/v1/night/session` every 2 seconds. That route needs no token. The Follow Show Night policy arms from preparing through fading out and disarms when the night is inactive or stopped. The tray and the settings window show whether the coordinator is connected.

If the coordinator stops answering after it has answered once, the companion keeps the last Show Night state it observed and says it is holding. A coordinator outage in the middle of a night therefore leaves Resolume supervision armed. The hold ends when the coordinator answers again, when an operator sets an override, or when the address is changed. A coordinator that has never answered leaves the night unknown, which is disarmed. The held state is not saved, so a companion restart during an outage starts from unknown.

The companion does not report anything back to the coordinator yet. ShowMesh cannot see whether the companion is running.

## Development

Requirements are Node.js, npm, Rust, and the platform prerequisites from the Tauri 2 documentation.

```sh
npm ci
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri dev
```

The macOS deployment target is 11.0 (Big Sur). Windows 10 remains the required Windows floor. Neither floor has been verified with a packaged artifact on a real machine, and compilation is not evidence of platform support.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the runtime shape and the boundaries, and [docs/BENCH-VERIFICATION.md](docs/BENCH-VERIFICATION.md) for the hardware checklist.

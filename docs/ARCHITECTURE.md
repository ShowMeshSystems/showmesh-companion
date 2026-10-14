# Architecture

ShowMesh Companion is an endpoint agent, not a second show controller. ShowMesh remains the source of show state; the companion applies local policy and reports observed facts.

Everything described here is implemented and unit-tested. None of it has been observed running on real hardware. Where this document describes behavior, read it as the behavior the code is written to produce, not as behavior anyone has watched happen.

## Runtime shape

The Tauri host owns settings, arming state, module lifecycle, and capability derivation. The webview is a local settings and status surface. Users should never need to construct or edit a configuration file.

Four threads run beside the Tauri main thread.

- **The supervisor poll thread** wakes every 2 seconds. It reads settings, night state, and the arming override, resolves the arming decision, refreshes the process list, and runs one supervision tick. It also rewrites the four tray status lines. It never holds the tray handle lock while calling into the menu, because `set_text` blocks on the main thread and the main thread is where menu clicks are handled; holding the lock across that call deadlocked the application.
- **The audio capture thread** owns the cpal input stream. It has to: a `cpal::Stream` is not `Send`, so it cannot be created on one thread and stored somewhere shared. The thread receives configure, disable, and shutdown commands over a channel, and on a 300 millisecond timeout it retries a stream that failed to open, so an interface that appears later is picked up without operator action.
- **The status ticker** emits a `timecode-status` event at 10 Hz. The settings window updates the live timecode readout by writing to specific DOM nodes on that event. It must never re-render the page from the tick: a full re-render at 10 Hz would destroy focus and in-progress edits in the settings form.
- **The device enumeration refresher** rebuilds the audio input list every 2 seconds into a cache. Enumeration is done off the main thread because it can block. Until the first enumeration completes, the cache reports "not enumerated yet" rather than an empty list, so a configured device is never called missing merely because nothing has been listed.

Inside the capture thread, the realtime audio callback does no allocation and takes no blocking lock. It writes timestamps and the smoothed input level through atomics, and publishes the decoded frame and detected rate through a `try_lock` it is allowed to lose. Losing one publish costs one frame of freshness; blocking the callback would cost audio.

Modules implement independent observation concerns:

- `process.supervise` observes a configured local process and, when armed, may launch it if absent. It never kills or forcibly restarts the application.
- `timecode.ltc.observe` captures and decodes LTC from a configured local audio input. It reports input evidence only, not application-consumption evidence.

The application derives capability candidates from complete configuration. Actual advertisement will also require a successful runtime probe and controller connection. Disabled, incomplete, unavailable, and unobserved states must not be collapsed into one state.

## Arming

The default policy follows the Show Night session. Preparation through fade-out is armed; inactive and stopped are disarmed. Unknown controller state is disarmed rather than guessed. A local operator can temporarily arm for the current night or disarm until the next night boundary, from the settings window or from the tray, through one shared code path so the two surfaces cannot drift.

Night state comes from the coordinator (see Transport). Overrides expire against observed night boundaries: arm-until-night-ends clears when the night becomes inactive or stopped, and disarm-until-next-night clears when the next night enters preparing. With no coordinator, an override ends only when it is cleared.

## Process supervision

Supervision is a state machine over four phases: idle, launching, running, and backoff. A tick observes the process list and decides one action.

Matching prefers the configured application path. On macOS a configured `.app` bundle matches any executable inside it. A process that matches only by executable name is reported as present but not operational, and blocks a launch: the companion will not start a second copy of something it cannot positively identify.

- A launch is attempted only when the module is enabled, the path exists, the process is absent, and the decision is armed.
- After a launch the supervisor waits a 20 second grace window for a matching process to appear. If none appears, the attempt is a failure.
- A launched process that disappears inside a 60 second stability window is also a failure, which is what stops a crash-looping application from being relaunched every poll.
- Failures back off exponentially from 2 seconds, capped at 60 seconds, and retry forever. There is no attempt limit and no give-up state.
- Two launches can never happen inside 10 seconds of each other, regardless of how the state machine was reset.

macOS launches through `open -a` so the bundle is started the way the Finder would start it. Windows spawns the configured executable directly.

## Timecode observation

The decoder consumes one channel of the input stream, recovers the LTC biphase-mark bitstream, and emits frames on the sync word. The frame rate is detected from the wire by measuring the sample interval between decoded frames over a window of up to 96 frames, and drop frame is taken from the flag in the bitstream. Detection needs at least 60 intervals before it will report a rate at all, because 29.97 and 30 differ by one part in a thousand and cannot be separated from a short sample.

Status distinguishes: disabled, not configured, device unavailable, channel unavailable, no audio, audio but no lock, locked, and stream failure. These are separate states on purpose. "No audio at all" and "audio arriving but nothing decoding" send an operator to different cables.

## Settings and state on disk

Settings and the arming override are written to the application config directory through a staging file and a rename, and the file being replaced is retained as one previous generation (`settings.json.previous`, `state.json.previous`). The retained generation is what the recovery path reads.

On load, a file that will not parse, or that parses into values that fail validation, is moved aside as `.invalid`. The previous generation is used if it parses and validates; otherwise defaults are loaded. Either way the settings window is told what happened, so a silently reset configuration is not possible.

## Transport

The companion reads from the coordinator and does not report to it.

A fifth thread polls `GET /api/v1/night/session` on the configured address every 2 seconds, with a 3 second timeout. The route is open to unauthenticated reads, so the companion holds no credential. `session.state` maps one to one onto the companion's night states; a state the companion does not recognise is a failed poll, never a guess. The poll runs on its own thread so a slow coordinator cannot delay a supervision tick.

`CoordinatorLink` in `coordinator.rs` records the last observed state per address. A failed poll after a successful one holds that state and reports how long the coordinator has been silent. A failed poll before any success leaves the night unknown. Changing the address discards everything observed from the old one. The held state lives in memory only.

Registration, health, capability advertisement, and observation reporting are not implemented. They need an enrollment model and explicit schemas in the core repository, and the domain model here must not depend on MQTT topics or any single broker library when they arrive.

### Expected LTC frame rate

The coordinator holds the show's LTC rate in `audio.settings.ltcFrameRate`, but `GET /api/v1/config/audio.settings` requires the `config:write` scope. A companion should not hold a credential that can change show configuration, so the expected rate stays unobserved until the core repository offers a read the companion can use.

## Accepted design decisions

These were decided deliberately. Reopening them should be a decision, not a drive-by change.

- **LTC frame rate is detected from the wire and never configured locally.** There is no frame rate setting and there should not be one. An operator-entered rate would be one more thing to get wrong, and the wire already carries the answer.
- **The expected frame rate stays explicitly unobserved until the coordinator offers a read the companion can use.** The companion can detect what it is receiving, but it has no trustworthy statement of what it should be receiving. So it reports the comparison as "unknown, expected rate unobserved" rather than comparing against a guess. A mismatch will be reported when, and only when, the coordinator publishes the show's configured rate.
- **Supervision retries forever, with backoff capped at 60 seconds.** A companion that gave up after some number of attempts would be silently absent on the night it was needed. Backoff bounds the damage of a persistent failure without ever ending the attempt.
- **There is no kill, stop, or forced-restart control.** Supervision starts what is absent. It does not take anything away from an operator who is using it.
- **A lost coordinator holds the last observed night state.** Disarming on a coordinator outage would stop supervision exactly when the control plane is down, which contradicts the rule that a running show survives coordinator loss. A coordinator that has never answered still resolves to unknown and disarmed.
- **A configured audio device that is absent never falls back to the default input.** Falling back would produce confident, wrong timecode evidence from the wrong hardware. The companion reports the device as unavailable and stays quiet.

## Evidence and release gates

Local process presence, LTC received, coordinator connectivity, and target application behavior are separate evidence layers. The UI and protocol must preserve that distinction. Concretely: a matched process is not a working Resolume, a decoded LTC frame is not a consumed LTC frame, and a configured coordinator address is not a connection.

There is a fifth layer that this document must not blur: implemented is not verified. The modules above are unit-tested against synthesized input. They have not run against Resolume, an audio interface, an LTC generator, or a packaged installer.

Before a release can claim platform support, retained installers must be installed and exercised on macOS 11 and a current macOS release, plus Windows 10 and 11. Signing, notarization, code signing, upgrade behavior, login launch, crash recovery, and hardware audio inputs are independent acceptance gates. `docs/BENCH-VERIFICATION.md` holds the checklist, including the specific failure modes code review flagged and code cannot rule out.

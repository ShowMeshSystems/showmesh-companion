# Architecture

ShowMesh Companion is an endpoint agent, not a second show controller. ShowMesh remains the source of show state; the companion applies local policy and reports observed facts.

## Runtime shape

The Tauri host owns settings, arming state, module lifecycle, and capability derivation. The webview is a local settings and status surface. Users should never need to construct or edit a configuration file.

Modules implement independent observation concerns:

- `process.supervise` observes a configured local process and, when armed, may launch it if absent. It never kills or forcibly restarts the application.
- `timecode.ltc.observe` captures and decodes LTC from a configured local audio input. It reports input evidence only, not application-consumption evidence.

The foundation derives capability candidates from complete configuration. Actual advertisement will also require a successful runtime probe and controller connection. Disabled, incomplete, unavailable, and unobserved states must not be collapsed into one state.

## Arming

The default policy follows the Show Night session. Preparation through fade-out is armed; inactive and stopped are disarmed. Unknown controller state is disarmed rather than guessed. A local operator can temporarily arm for the current night or disarm until the next night boundary.

## Transport

Transport is deliberately behind the module/state boundary. The first implementation may use the coordinator's existing node transport, but the domain model must not depend on MQTT topics or any single broker library. Registration, health, capability advertisement, and observations need explicit schemas and reconnect behavior.

## Evidence and release gates

Local process presence, LTC received, coordinator connectivity, and target application behavior are separate evidence layers. The UI and protocol must preserve that distinction.

Before a release can claim platform support, retained installers must be installed and exercised on macOS 11 and a current macOS release, plus Windows 10 and 11. Signing, notarization, code signing, upgrade behavior, login launch, crash recovery, and hardware audio inputs are independent acceptance gates.

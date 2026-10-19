# Bench verification checklist

This is a release gate. Nothing in ShowMesh Companion has been observed running: it was written and unit-tested with no Resolume install, no audio interface, no LTC source, and no GUI session. Until the checks below are worked through on real machines, the application is unproven, and the documentation says so.

Work through it with the built application, not with `npm run tauri dev`, except where a check explicitly says a development build is acceptable.

## How to use this document

- Each check has a pass condition. Record pass, fail, or not run, with the build version, the machine, and the OS version.
- A check that cannot be run is not a pass. Record it as not run and say why.
- Several checks below exist because code review found a specific risk that code cannot rule out. Those are marked **Risk**. Do not skip them because the feature "obviously works".

## macOS

Run the whole section twice: once on macOS 11.0, the deployment floor, and once on a current macOS release.

| # | Check | Pass condition |
| --- | --- | --- |
| M1 | **Risk: tray deadlock.** Open the tray menu and leave it open for at least 10 seconds, so several 2 second supervisor polls elapse while the menu is up. Then click an override item. Repeat at least 20 times, including with the settings window open and with Resolume running. | The click applies every time. The application never hangs, never beachballs, and the tray menu keeps updating afterwards. |
| M2 | **Risk: activation under `Accessory`.** Hide the window completely, bring another application to full screen, then click Open settings in the tray. | The settings window comes to the front over the full-screen application, takes keyboard focus, and accepts typing on the first click, with no second click and no click into the window first. |
| M3 | **Risk: no menu bar means no key equivalents.** With focus in the Coordinator address field, use Cmd+A, Cmd+C, Cmd+V, Cmd+X, and Cmd+Z. Repeat in the Companion name and application path fields. | All five work. In particular, a coordinator URL can be pasted from the clipboard. If this fails, an operator cannot paste a URL, and the application needs an edit menu. |
| M4 | Menu bar icon appearance: light menu bar, dark menu bar, and with Reduce Transparency enabled. Check on a Retina and a non-Retina display if both are available. | The icon is legible and correctly shaped in every combination, with no filled box, halo, or clipping. |
| M5 | Launch the application and look at the Dock and the app switcher. | No Dock icon, no app switcher entry. |
| M6 | Close the settings window with the red button and with Cmd+W. Wait 60 seconds, then reopen from the tray. | The process is still running and still polling: the tray status lines have changed or are current. The window reopens and takes focus. |
| M7 | Set a tray override, quit from the tray, relaunch. | The override is still in force, shown in both the tray and the settings window. |
| M8 | **Risk: unwritable configuration directory.** Make the application config directory read-only, then click a tray override. | The operator gets visible feedback that the override could not be saved. The application does not silently report the new override while failing to persist it. |
| M9 | Change the arming override from the tray while the settings window is open, and again while it is hidden then reopened. | The settings window shows the new override immediately in the first case, and on reopening in the second. It never shows a stale arming state. |
| M10 | Point the application path at a real Resolume Arena bundle, arm, and quit Resolume. | Resolume is relaunched within roughly one poll plus launch time, once, not repeatedly. |

## Windows

Run the whole section twice: once on Windows 10, the deployment floor, and once on Windows 11.

| # | Check | Pass condition |
| --- | --- | --- |
| W1 | **Risk: duplicate instance over a network share.** Point the application path at a Resolume executable on a network share, arm, and induce a share dropout while Resolume is starting, so the path check and the process check disagree. Repeat several times with different dropout timings. | Exactly one Resolume process exists at all times. A second Resolume never appears. |
| W2 | **Risk: tray deadlock.** As M1: hold the tray menu open across several polls, then click an override, at least 20 times, with the settings window both open and closed. | Every click applies. No hang. |
| W3 | Close the settings window. | The taskbar button disappears. Reopening from the tray restores the window and it takes foreground, on the first click. |
| W4 | **Risk: elevated target process.** Run Resolume elevated (as administrator) with the companion not elevated. Inspect the reported match type in the settings window. | The match is reported as matched by path. If it degrades to matched by name, note it: name-only matching never launches and never reports operational, so supervision silently does nothing useful for every elevated Resolume. Decide whether the companion must also run elevated before release. |
| W5 | **Risk: grace window too short.** Time Resolume from launch to first appearing in the process list, with a real show composition loaded, on the slowest machine in the fleet. Repeat at least five times, cold and warm. | The time is comfortably under the 20 second grace window. If it routinely exceeds it, every launch is scored as a failure and backoff will grow, so the grace window must be raised before release. |
| W6 | Quit Resolume with the companion armed. | Resolume is relaunched once, not repeatedly. |
| W7 | Set a tray override, quit, relaunch. | The override is still in force. |
| W8 | Make the configuration directory unwritable, then click a tray override. | The operator gets visible feedback that the override could not be saved. |

## Coordinator

Run on either platform against a real coordinator, with the policy set to Follow Show Night and Resolume supervision enabled.

| # | Check | Pass condition |
| --- | --- | --- |
| C1 | Enter the coordinator address and save, with no night running. | Within a few seconds the tray and settings window show the coordinator connected and Show Night inactive, and supervision is disarmed. |
| C2 | Start a night on the coordinator. | Show Night follows each state within a few seconds. Supervision arms at preparing. |
| C3 | **Risk: coordinator loss mid-night.** With the night live, stop the coordinator or unplug its network. Quit Resolume. | The companion reports the coordinator unreachable and Show Night held at live, stays armed, and relaunches Resolume once. |
| C4 | Bring the coordinator back. | The hold ends and Show Night tracks the coordinator again. |
| C5 | Arm until the night ends from the tray, then end the night on the coordinator. | The override clears when the night reaches stopped or inactive, and supervision disarms. |
| C6 | Enter an address where nothing is listening. | The companion reports it cannot reach the coordinator, Show Night stays unknown, and supervision is disarmed. |

## Crash reports

Run on macOS 11 and a current macOS, against a real coordinator that serves `POST /api/v1/companion-reports`, with a token issued for a machine principal holding the `companion` role.

| # | Check | Pass condition |
| --- | --- | --- |
| R1 | Save the coordinator address and token with Crash reports on, then copy a real Arena crash report (or any small text file named like `Arena-2026-10-14-205842.ips`) into `~/Library/Logs/DiagnosticReports`. Wait at least 35 seconds. | The tray and settings window show one more report sent. `showmeshctl companion-reports list` on the coordinator lists it under this companion's name, with the right file name and capture time. |
| R2 | Quit and relaunch the companion. Wait 35 seconds. | The sent count is unchanged and the coordinator holds no duplicate. |
| R3 | Let Resolume crash for real with the companion running, so Resolume's own crash window appears. | Within about a minute the coordinator lists `lastCrash.log` and `lastCrash.mem.txt` from this companion, with a capture time matching the crash, and their content matches `unzip -p ~/Library/Caches/com.resolume.arena/lastCrash.zip lastCrash.log`. If macOS also wrote an `Arena-*.ips`, it arrives as well. |
| R4 | Stop the coordinator, add a crash report, wait one scan, then start the coordinator. | The companion reports the coordinator unreachable, then sends the report once the coordinator answers. |
| R5 | Revoke the token on the coordinator, then add a crash report. | The companion reports the token rejected. After pasting a new token and saving, the report is sent. |
| R6 | Reopen the settings window after saving a token. | The token field is empty and says a token is saved. Saving other changes with the field blank keeps the token working. Ticking Remove the saved token stops uploads and the module reports no token. |
| R7 | **Risk: folder permissions.** On a current macOS, confirm the companion can list `~/Library/Logs/DiagnosticReports` without extra privacy permissions. | Reports are found. If the folder cannot be read, the module says so; record which macOS needs which permission. |
| R8 | `ls -l` the settings file in the application config directory. | It is readable and writable only by its owner. |

## Audio and timecode

Run on both platforms, with a real audio interface and a real LTC generator.

| # | Check | Pass condition |
| --- | --- | --- |
| A1 | Feed known LTC at 24, 25, 29.97 drop, and 30 in turn. | The displayed value matches the generator, reading the same frame at the same moment, in all four cases. |
| A2 | For each rate in A1, check the detected frame rate field. Leave each rate running for at least 5 seconds, since detection needs at least 60 frames. | The detected rate is correct. 29.97 and 30 are reported distinctly and are never confused with each other. Drop frame is reported correctly for 29.97 drop. |
| A3 | Select a channel that carries no LTC on a device that is receiving other audio. | Reported as audio present with no lock. Not reported as silence, and not reported as locked. |
| A4 | Select a channel with nothing connected, so the input is silent. | Reported as no audio, distinctly from A3. |
| A5 | Unplug the interface while capture is running. | Reported as device unavailable or stream failure. No timecode continues to be reported. Capture never falls back to the built-in microphone or any other input. |
| A6 | Plug the interface back in. | Capture resumes on its own within a few seconds, with no operator action. |
| A7 | Stop the LTC generator while locked. | Loss of lock is reported within about a quarter of a second. The last decoded frame is never presented as current. |
| A8 | Run the whole of A1 at 48 kHz and again at 44.1 kHz. | Decoding and rate detection are correct at both. |
| A9 | Set the interface to a sample rate the application did not open the stream with, or change the interface's rate while running. | The application either follows the change or reports a failure. It never reports plausible but wrong timecode. |
| A10 | Sleep the machine with capture running and locked, then wake it. | Capture recovers, or reports a clear failure state. It does not sit reporting stale lock. |
| A11 | Select an audio device, quit, unplug it, relaunch. | Reported as device unavailable. Not silently swapped for another device. |
| A12 | Leave capture locked and running for at least 8 hours. | Still locked, still accurate against the generator, with no drift in reported values and no growth in memory. |

## Packaging and platform support

Per AGENTS.md, platform support cannot be claimed from compilation. Each row is an independent gate, and each must be exercised from the retained installer, on macOS 11, a current macOS, Windows 10, and Windows 11.

| # | Check | Pass condition |
| --- | --- | --- |
| P1 | Install from the retained installer on a clean machine. | Installs and launches. |
| P2 | Code signing. | The signature is valid and the OS raises no warning on first launch. |
| P3 | Notarization (macOS). | Notarized and stapled; Gatekeeper allows first launch with no override. |
| P4 | Upgrade over an existing install. | Settings and the saved arming override survive the upgrade. |
| P5 | Launch at login. | **Cannot pass today.** `launchAtLogin` is stored in settings and nothing acts on it: the application does not register itself with the operating system. This gate stays open until login registration is implemented. |
| P6 | Crash recovery. | After a forced kill, the next launch loads settings and the saved override, or reports clearly why it could not. |
| P7 | Uninstall. | Removes the application. Note what it leaves behind in the config directory. |

## Known gaps that no check here can close

- The companion does not report to the coordinator. ShowMesh cannot show its state.
- The expected LTC frame rate is unobserved, because the coordinator only serves the show's configured rate to a `config:write` credential. Frame rate comparison therefore never reports a mismatch.
- Capability candidates are derived but never advertised.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type ArmingPolicy = "follow-show-night" | "always-armed" | "always-disarmed";
type ArmingOverride = "none" | "arm-until-night-ends" | "disarm-until-next-night";

interface Settings {
  companionName: string;
  coordinatorUrl: string;
  armingPolicy: ArmingPolicy;
  launchAtLogin: boolean;
  resolume: { enabled: boolean; applicationPath: string };
  timecode: { enabled: boolean; deviceId: string; channel: number };
}

interface AudioInputDevice {
  id: string;
  name: string;
  channels: number | null;
  defaultSampleRate: number | null;
  configKnown: boolean;
}

type TimecodeState =
  | "disabled"
  | "not-configured"
  | "not-started"
  | "device-unavailable"
  | "channel-unavailable"
  | "no-audio"
  | "no-lock"
  | "locked"
  | "decode-failed";

interface ObservedFrame {
  hours: number;
  minutes: number;
  seconds: number;
  frames: number;
  dropFrame: boolean;
}

interface TimecodeStatus {
  state: TimecodeState;
  reason: string;
  operational: boolean;
  level: number | null;
  frame: ObservedFrame | null;
  frameAgeMs: number | null;
  detectedFrameRate: string | null;
  frameRateComparison: "match" | "mismatch" | "unknown-expected-unobserved";
}

interface Snapshot {
  settings: Settings;
  capabilities: Array<{ id: string; version: number }>;
  nightState: string;
  armingOverride: ArmingOverride;
  effectiveArmed: boolean;
  armingReason: string;
  supervisor: {
    state: string;
    reason: string;
    operational: boolean;
    consecutiveFailures: number;
    lastError: string | null;
    matchedBy: string;
  };
  timecode: TimecodeStatus;
  settingsNotice: string | null;
}

function requireAppRoot(): HTMLElement {
  const root = document.querySelector<HTMLElement>("#app");
  if (!root) throw new Error("app root is missing");
  return root;
}

const app = requireAppRoot();

let snapshot: Snapshot | null = null;
let draft: Settings | null = null;
let error: string | null = null;
let saving = false;
let audioDevices: AudioInputDevice[] = [];
let audioDevicesError: string | null = null;
let audioDevicesLoading = false;

function escapeHtml(value: string): string {
  return value.replace(/[&<>'"]/g, (character) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;",
  })[character] ?? character);
}

function readForm(): Settings | null {
  const form = document.querySelector<HTMLFormElement>("#settings-form");
  if (!form) return null;
  const data = new FormData(form);
  return {
    companionName: String(data.get("companionName") ?? ""),
    coordinatorUrl: String(data.get("coordinatorUrl") ?? ""),
    armingPolicy: String(data.get("armingPolicy")) as ArmingPolicy,
    launchAtLogin: data.has("launchAtLogin"),
    resolume: { enabled: data.has("resolumeEnabled"), applicationPath: String(data.get("applicationPath") ?? "") },
    timecode: { enabled: data.has("timecodeEnabled"), deviceId: String(data.get("deviceId") ?? ""), channel: Number(data.get("channel") ?? 1) },
  };
}

// Any control inside the form re-renders the page, which would discard edits the
// operator has typed but not saved. Capture them first so the re-render restores them.
function captureDraft(): void {
  const current = readForm();
  if (current) draft = current;
}

function render(): void {
  if (!snapshot) {
    app.innerHTML = `<div class="loading"><p class="eyebrow">ShowMesh Companion</p><h1>${error ? "Could not open settings" : "Opening companion…"}</h1><p>${escapeHtml(error ?? "Reading local settings.")}</p>${error ? '<button id="retry" class="button secondary">Try again</button>' : ""}</div>`;
    document.querySelector("#retry")?.addEventListener("click", load);
    return;
  }

  const settings = draft ?? snapshot.settings;
  const capabilities = snapshot.capabilities.length
    ? snapshot.capabilities.map((capability) => `<li><span>${escapeHtml(capability.id)}</span><small>v${capability.version}</small></li>`).join("")
    : '<li class="empty">No capability candidates exist until a module is configured.</li>';

  app.innerHTML = `
    <header class="app-header">
      <div>
        <p class="eyebrow">ShowMesh Companion</p>
        <h1>${escapeHtml(settings.companionName)}</h1>
      </div>
      <div class="arm-state ${snapshot.effectiveArmed ? "armed" : "disarmed"}">
        <span class="state-dot" aria-hidden="true"></span>
        <div><small>Supervision</small><strong>${snapshot.effectiveArmed ? "Armed" : "Disarmed"}</strong></div>
      </div>
    </header>

    ${snapshot.settingsNotice ? `<p class="form-error" role="alert">${escapeHtml(snapshot.settingsNotice)}</p>` : ""}

    <section class="status-band" aria-label="Current state">
      <div><span>Show Night</span><strong>${escapeHtml(snapshot.nightState)}</strong></div>
      <div><span>Process monitor</span><strong>${escapeHtml(snapshot.supervisor.state)}</strong></div>
      <div><span>LTC</span><strong id="status-timecode-state">${escapeHtml(snapshot.timecode.state)}</strong></div>
      <div><span>Controller</span><strong>${settings.coordinatorUrl ? "Configured" : "Not configured"}</strong></div>
      <p>${escapeHtml(snapshot.armingReason)}</p>
    </section>

    <form id="settings-form">
      <section class="section">
        <div class="section-heading"><div><p class="eyebrow">Connection</p><h2>Identity and controller</h2></div><p>Settings stay on this computer. No configuration file editing is required.</p></div>
        <div class="field-grid">
          <label><span>Companion name</span><input name="companionName" required value="${escapeHtml(settings.companionName)}" /></label>
          <label><span>Coordinator address</span><input name="coordinatorUrl" type="url" placeholder="http://showmesh.local:8080" value="${escapeHtml(settings.coordinatorUrl)}" /></label>
        </div>
        <label class="check"><input name="launchAtLogin" type="checkbox" ${settings.launchAtLogin ? "checked" : ""}/><span><strong>Open at login</strong><small>This preference is saved, but the application does not register itself with the operating system yet.</small></span></label>
      </section>

      <section class="section">
        <div class="section-heading"><div><p class="eyebrow">Policy</p><h2>When supervision is armed</h2></div><p>Following Show Night arms during preparation and stays armed through fade-out.</p></div>
        <div class="segmented" role="radiogroup" aria-label="Arming policy">
          ${policyOption("follow-show-night", "Follow Show Night", settings.armingPolicy)}
          ${policyOption("always-armed", "Always armed", settings.armingPolicy)}
          ${policyOption("always-disarmed", "Always disarmed", settings.armingPolicy)}
        </div>
        <div class="override-row">
          <span><strong>Temporary override</strong><small>${escapeHtml(snapshot.armingOverride)}</small></span>
          <button type="button" class="button secondary" data-override="arm-until-night-ends">Arm for this night</button>
          <button type="button" class="button secondary" data-override="disarm-until-next-night">Disarm until next night</button>
          <button type="button" class="button ghost" data-override="none">Clear</button>
        </div>
        <p class="module-notice">A temporary override is saved on this computer and stays in force after a restart. Expiry depends on Show Night state, which has not been observed yet, so an override stays in force until it is cleared, either here or from the tray menu.</p>
      </section>

      <section class="section modules">
        <div class="section-heading"><div><p class="eyebrow">Modules</p><h2>Local capabilities</h2></div><p>Only configured modules are eligible to advertise a capability.</p></div>
        <article class="module">
          <div class="module-title"><div><h3>Resolume supervision</h3><p>Observe Arena locally and launch it only when absent and armed.</p></div><label class="switch"><input name="resolumeEnabled" type="checkbox" ${settings.resolume.enabled ? "checked" : ""}/><span aria-hidden="true"></span><b>Enabled</b></label></div>
          <label><span>Resolume Arena application</span><input name="applicationPath" placeholder="/Applications/Resolume Arena/Arena.app" value="${escapeHtml(settings.resolume.applicationPath)}" /></label>
          <p class="module-notice">A running process is observed evidence only. It is not proof that Resolume is operating correctly.</p>
          <dl class="detail-grid">
            <div><dt>State</dt><dd>${escapeHtml(snapshot.supervisor.state)}</dd></div>
            <div><dt>Matched by</dt><dd>${escapeHtml(snapshot.supervisor.matchedBy)}</dd></div>
            <div><dt>Consecutive failures</dt><dd>${snapshot.supervisor.consecutiveFailures}</dd></div>
            <div><dt>Last OS error</dt><dd>${snapshot.supervisor.lastError ? escapeHtml(snapshot.supervisor.lastError) : "None"}</dd></div>
          </dl>
        </article>
        <article class="module">
          <div class="module-title"><div><h3>LTC observation</h3><p>Decode timecode received on this computer without claiming an application consumed it.</p></div><label class="switch"><input name="timecodeEnabled" type="checkbox" ${settings.timecode.enabled ? "checked" : ""}/><span aria-hidden="true"></span><b>Enabled</b></label></div>
          <div class="field-grid compact">
            <label><span>Audio input</span><select name="deviceId">${deviceOptions(audioDevices, settings.timecode.deviceId)}</select></label>
            <label><span>Channel</span><input name="channel" type="number" min="1" ${selectedDeviceChannelMax(audioDevices, settings.timecode.deviceId)} value="${settings.timecode.channel}" /></label>
          </div>
          <div class="device-actions">
            <button type="button" class="button secondary" id="refresh-devices" ${audioDevicesLoading ? "disabled" : ""}>${audioDevicesLoading ? "Refreshing…" : "Refresh device list"}</button>
          </div>
          ${audioDevicesError ? `<p class="form-error" role="alert">${escapeHtml(audioDevicesError)}</p>` : ""}
          ${timecodeLive(snapshot.timecode)}
        </article>
      </section>

      <section class="section capability-section">
        <div class="section-heading"><div><p class="eyebrow">Configuration preview</p><h2>Capability candidates</h2></div><p>These are not advertised. A successful runtime probe and controller connection will be required first.</p></div>
        <ul class="capabilities">${capabilities}</ul>
      </section>

      ${error ? `<p class="form-error" role="alert">${escapeHtml(error)}</p>` : ""}
      <footer><p>${escapeHtml(snapshot.supervisor.reason)}</p><button class="button primary" type="submit" ${saving ? "disabled" : ""}>${saving ? "Saving…" : "Save settings"}</button></footer>
    </form>`;

  document.querySelector<HTMLFormElement>("#settings-form")?.addEventListener("submit", save);
  document.querySelectorAll<HTMLButtonElement>("[data-override]").forEach((button) => {
    button.addEventListener("click", () => {
      captureDraft();
      void setOverride(button.dataset.override as ArmingOverride);
    });
  });
  document
    .querySelector<HTMLSelectElement>('select[name="deviceId"]')
    ?.addEventListener("change", handleDeviceChange);
  document
    .querySelector<HTMLButtonElement>("#refresh-devices")
    ?.addEventListener("click", () => {
      captureDraft();
      void loadAudioDevices();
    });
}

function handleDeviceChange(event: Event): void {
  const select = event.currentTarget as HTMLSelectElement;
  const device = audioDevices.find((candidate) => candidate.id === select.value);
  const channelInput = document.querySelector<HTMLInputElement>('input[name="channel"]');
  if (!channelInput) return;
  if (device?.channels) {
    channelInput.max = String(device.channels);
  } else {
    channelInput.removeAttribute("max");
  }
}

function deviceOptions(devices: AudioInputDevice[], selectedId: string): string {
  const known = devices.some((device) => device.id === selectedId);
  const options: string[] = [];
  if (!selectedId) {
    options.push('<option value="" selected>Choose an audio input…</option>');
  } else if (!known) {
    options.push(
      `<option value="${escapeHtml(selectedId)}" selected>${escapeHtml(selectedId)} (not currently available)</option>`,
    );
  }
  for (const device of devices) {
    const detail = !device.configKnown
      ? " (configuration unavailable)"
      : device.channels !== null
        ? ` (${device.channels} channel${device.channels === 1 ? "" : "s"})`
        : "";
    options.push(
      `<option value="${escapeHtml(device.id)}" ${device.id === selectedId ? "selected" : ""}>${escapeHtml(device.name)}${detail}</option>`,
    );
  }
  if (!options.length) {
    options.push('<option value="">No audio input devices were found.</option>');
  }
  return options.join("");
}

function selectedDeviceChannelMax(devices: AudioInputDevice[], selectedId: string): string {
  const device = devices.find((candidate) => candidate.id === selectedId);
  return device?.channels ? `max="${device.channels}"` : "";
}

function policyOption(value: ArmingPolicy, label: string, selected: ArmingPolicy): string {
  return `<label><input type="radio" name="armingPolicy" value="${value}" ${value === selected ? "checked" : ""}/><span>${label}</span></label>`;
}

function pad2(value: number): string {
  return String(value).padStart(2, "0");
}

function formatTimecodeValue(frame: ObservedFrame | null): string {
  if (!frame) return "--:--:--:--";
  return `${pad2(frame.hours)}:${pad2(frame.minutes)}:${pad2(frame.seconds)}:${pad2(frame.frames)}`;
}

function formatFrameRate(rate: string | null): string {
  return rate ? `${rate} fps` : "Not detected";
}

function formatDropFrame(frame: ObservedFrame | null): string {
  if (!frame) return "Unknown";
  return frame.dropFrame ? "Drop frame" : "Non-drop frame";
}

function formatAge(ms: number | null): string {
  if (ms === null) return "No frame decoded yet";
  if (ms < 1000) return `${ms} ms ago`;
  return `${(ms / 1000).toFixed(1)} s ago`;
}

function formatLevel(level: number | null): string {
  if (level === null) return "Unknown";
  return level.toFixed(3);
}

function timecodeLive(status: Snapshot["timecode"]): string {
  return `
    <div class="timecode-live" id="timecode-live" data-state="${status.state}">
      <div class="timecode-value" id="tc-value">${formatTimecodeValue(status.frame)}</div>
      <dl class="timecode-meta">
        <div><dt>Lock state</dt><dd id="tc-state">${escapeHtml(status.state)}</dd></div>
        <div><dt>Frame rate</dt><dd id="tc-rate">${formatFrameRate(status.detectedFrameRate)}</dd></div>
        <div><dt>Drop frame</dt><dd id="tc-drop">${formatDropFrame(status.frame)}</dd></div>
        <div><dt>Last frame</dt><dd id="tc-age">${formatAge(status.frameAgeMs)}</dd></div>
        <div><dt>Input level</dt><dd id="tc-level">${formatLevel(status.level)}</dd></div>
      </dl>
      <p class="module-notice" id="tc-reason">${escapeHtml(status.reason)}</p>
      <p class="timecode-caveat">This is timecode received by this computer. It is not evidence that Resolume, or any other consumer, received or followed it.</p>
    </div>`;
}

function setText(selector: string, value: string): void {
  const element = document.querySelector<HTMLElement>(selector);
  if (element) element.textContent = value;
}

// Called from the 10 Hz "timecode-status" event. Must never call render(): a full
// re-render on every tick would destroy focus and in-progress edits in the settings form.
function updateTimecodeLive(status: TimecodeStatus): void {
  if (snapshot) snapshot.timecode = status;
  const container = document.querySelector<HTMLElement>("#timecode-live");
  if (container) container.dataset.state = status.state;
  setText("#tc-value", formatTimecodeValue(status.frame));
  setText("#tc-state", status.state);
  setText("#tc-rate", formatFrameRate(status.detectedFrameRate));
  setText("#tc-drop", formatDropFrame(status.frame));
  setText("#tc-age", formatAge(status.frameAgeMs));
  setText("#tc-level", formatLevel(status.level));
  setText("#tc-reason", status.reason);
  setText("#status-timecode-state", status.state);
}

async function load(): Promise<void> {
  error = null;
  render();
  try {
    snapshot = await invoke<Snapshot>("get_snapshot");
  } catch (cause) {
    error = String(cause);
  }
  render();
}

async function loadAudioDevices(): Promise<void> {
  audioDevicesLoading = true;
  render();
  try {
    audioDevices = await invoke<AudioInputDevice[]>("list_audio_inputs");
    audioDevicesError = null;
  } catch (cause) {
    audioDevicesError = String(cause);
  }
  audioDevicesLoading = false;
  render();
}

async function save(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  if (!snapshot) return;
  const settings = readForm();
  if (!settings) return;
  saving = true;
  draft = settings;
  error = null;
  render();
  try {
    snapshot = await invoke<Snapshot>("save_settings", { settings });
    draft = null;
  } catch (cause) {
    error = String(cause);
  }
  saving = false;
  render();
}

async function setOverride(value: ArmingOverride): Promise<void> {
  error = null;
  try {
    snapshot = await invoke<Snapshot>("set_arming_override", { value });
  } catch (cause) {
    error = String(cause);
  }
  render();
}

// The tray can change arming state while this window is hidden, so the window
// must never render a snapshot it took when it was last visible.
function adoptSnapshot(next: Snapshot): void {
  captureDraft();
  snapshot = next;
  render();
}

async function refresh(): Promise<void> {
  try {
    const next = await invoke<Snapshot>("get_snapshot");
    error = null;
    adoptSnapshot(next);
  } catch (cause) {
    error = String(cause);
    render();
  }
}

window.addEventListener("focus", () => void refresh());
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) void refresh();
});

void load();
void loadAudioDevices();
void listen<TimecodeStatus>("timecode-status", (event) => updateTimecodeLive(event.payload));
void listen<Snapshot>("app-snapshot", (event) => adoptSnapshot(event.payload));

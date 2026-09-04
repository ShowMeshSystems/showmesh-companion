import { invoke } from "@tauri-apps/api/core";

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

interface Snapshot {
  settings: Settings;
  capabilities: Array<{ id: string; version: number }>;
  nightState: string;
  armingOverride: ArmingOverride;
  effectiveArmed: boolean;
  armingReason: string;
  supervisor: { state: string; reason: string; operational: boolean };
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

function escapeHtml(value: string): string {
  return value.replace(/[&<>'"]/g, (character) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;",
  })[character] ?? character);
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

    <section class="status-band" aria-label="Current state">
      <div><span>Show Night</span><strong>${escapeHtml(snapshot.nightState)}</strong></div>
      <div><span>Process monitor</span><strong>${snapshot.supervisor.operational ? "Observing" : "Not observing"}</strong></div>
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
        <label class="check"><input name="launchAtLogin" type="checkbox" ${settings.launchAtLogin ? "checked" : ""}/><span><strong>Open at login</strong><small>This preference is saved, but OS login registration is not operational in the foundation build.</small></span></label>
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
      </section>

      <section class="section modules">
        <div class="section-heading"><div><p class="eyebrow">Modules</p><h2>Local capabilities</h2></div><p>Only configured modules are eligible to advertise a capability.</p></div>
        <article class="module">
          <div class="module-title"><div><h3>Resolume supervision</h3><p>Observe Arena locally and launch it only when absent and armed.</p></div><label class="switch"><input name="resolumeEnabled" type="checkbox" ${settings.resolume.enabled ? "checked" : ""}/><span aria-hidden="true"></span><b>Enabled</b></label></div>
          <label><span>Resolume Arena application</span><input name="applicationPath" placeholder="/Applications/Resolume Arena/Arena.app" value="${escapeHtml(settings.resolume.applicationPath)}" /></label>
          <p class="module-notice">Foundation only: process observation and launching are not operational in this build.</p>
        </article>
        <article class="module">
          <div class="module-title"><div><h3>LTC observation</h3><p>Decode timecode received on this computer without claiming an application consumed it.</p></div><label class="switch"><input name="timecodeEnabled" type="checkbox" ${settings.timecode.enabled ? "checked" : ""}/><span aria-hidden="true"></span><b>Enabled</b></label></div>
          <div class="field-grid compact">
            <label><span>Audio input</span><input name="deviceId" placeholder="Device discovery is not implemented yet" value="${escapeHtml(settings.timecode.deviceId)}" /></label>
            <label><span>Channel</span><input name="channel" type="number" min="1" value="${settings.timecode.channel}" /></label>
          </div>
          <p class="module-notice">Foundation only: audio capture and LTC decoding are not operational in this build.</p>
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
    button.addEventListener("click", () => setOverride(button.dataset.override as ArmingOverride));
  });
}

function policyOption(value: ArmingPolicy, label: string, selected: ArmingPolicy): string {
  return `<label><input type="radio" name="armingPolicy" value="${value}" ${value === selected ? "checked" : ""}/><span>${label}</span></label>`;
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

async function save(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  if (!snapshot) return;
  const data = new FormData(event.currentTarget as HTMLFormElement);
  const settings: Settings = {
    companionName: String(data.get("companionName") ?? ""),
    coordinatorUrl: String(data.get("coordinatorUrl") ?? ""),
    armingPolicy: String(data.get("armingPolicy")) as ArmingPolicy,
    launchAtLogin: data.has("launchAtLogin"),
    resolume: { enabled: data.has("resolumeEnabled"), applicationPath: String(data.get("applicationPath") ?? "") },
    timecode: { enabled: data.has("timecodeEnabled"), deviceId: String(data.get("deviceId") ?? ""), channel: Number(data.get("channel") ?? 1) },
  };
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

void load();

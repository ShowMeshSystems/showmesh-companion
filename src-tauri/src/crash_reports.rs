use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SCAN_INTERVAL: Duration = Duration::from_secs(30);
pub const MAX_CONTENT_BYTES: u64 = 4 * 1024 * 1024;
const UPLOADS_PER_SCAN: usize = 5;
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(30);
const REPORTS_PATH: &str = "/api/v1/companion-reports";
const REPORT_KIND: &str = "crash_report";
const REPORT_SOURCE: &str = "resolume-arena";
const DEFAULT_EXECUTABLE: &str = "Arena";
const LEDGER_FILE: &str = "crash-reports.json";
/// Arena's own crash handler (BugSplat) catches most crashes before macOS
/// writes an `.ips` file, then saves its attachments here as one zip that is
/// replaced on every crash.
const BUGSPLAT_FOLDER: &str = "Library/Caches/com.resolume.arena";
const BUGSPLAT_ARCHIVE: &str = "lastCrash.zip";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CrashReportState {
    Disabled,
    NotConfigured,
    Unavailable,
    Watching,
    TokenRejected,
    CoordinatorUnreachable,
    FolderUnreadable,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashReportStatus {
    pub state: CrashReportState,
    pub reason: String,
    pub sent_count: usize,
    pub last_sent_at_ms: Option<u64>,
    pub last_error: Option<String>,
}

impl CrashReportStatus {
    pub fn starting() -> Self {
        Self {
            state: CrashReportState::NotConfigured,
            reason: "Crash report upload has not checked its settings yet.".into(),
            sent_count: 0,
            last_sent_at_ms: None,
            last_error: None,
        }
    }
}

/// What a scan needs from settings. Built fresh for every scan so a settings
/// change applies on the next one.
pub struct UploadConfig {
    pub enabled: bool,
    pub base_url: String,
    pub token: String,
    pub companion_id: String,
    pub executable_prefix: String,
}

#[derive(Debug, Eq, PartialEq)]
pub enum UploadOutcome {
    Accepted,
    Refused(String),
    TokenRejected,
    Retry(String),
}

pub struct Report<'a> {
    pub file_name: &'a str,
    pub captured_at: String,
    pub sha256: &'a str,
    pub content: &'a str,
}

/// Classifies a coordinator answer per the upload contract: accepted and refused
/// files are never sent again, everything else is retried on a later scan.
pub fn classify_status(code: u16) -> UploadOutcome {
    match code {
        200 | 201 => UploadOutcome::Accepted,
        400 => UploadOutcome::Refused("the coordinator refused it as invalid".into()),
        413 => UploadOutcome::Refused("the coordinator refused it as too large".into()),
        401 | 403 => UploadOutcome::TokenRejected,
        other => UploadOutcome::Retry(format!("the coordinator answered HTTP {other}")),
    }
}

/// Maps a companion name onto the contract's `[A-Za-z0-9._-]{1,64}`.
pub fn normalize_companion_id(name: &str) -> String {
    let mut id = String::new();
    for character in name.trim().chars() {
        let mapped = if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            character
        } else {
            '-'
        };
        if mapped == '-' && id.ends_with('-') {
            continue;
        }
        id.push(mapped);
    }
    let id: String = id.trim_matches('-').chars().take(64).collect();
    let id = id.trim_end_matches('-').to_string();
    if id.is_empty() {
        "companion".into()
    } else {
        id
    }
}

/// The executable name crash reports are filed under, taken from the configured
/// application path: `/Applications/Resolume Arena/Arena.app` gives `Arena`.
pub fn executable_prefix(application_path: &str) -> String {
    let components: Vec<&str> = application_path
        .trim()
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .collect();
    let chosen = components
        .iter()
        .find(|component| component.ends_with(".app"))
        .or(components.last());
    chosen
        .map(|name| match name.rsplit_once('.') {
            Some((stem, _)) if !stem.is_empty() => stem,
            _ => name,
        })
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(DEFAULT_EXECUTABLE)
        .to_string()
}

/// True for `Arena-2026-10-14-205842.ips` and `Arena_2026-10-14-205842_host.crash`,
/// false for another process whose name merely starts with the same letters.
pub fn is_candidate(file_name: &str, prefix: &str) -> bool {
    let Some(rest) = file_name.strip_prefix(prefix) else {
        return false;
    };
    let separated = rest.starts_with('-') || rest.starts_with('_');
    let extension_ok = file_name.ends_with(".ips") || file_name.ends_with(".crash");
    separated && extension_ok
}

pub fn report_folders() -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let home = PathBuf::from(home);
    let base = home.join("Library/Logs/DiagnosticReports");
    vec![base.join("Retired"), base, home.join(BUGSPLAT_FOLDER)]
}

#[derive(Clone, Debug)]
struct Candidate {
    path: PathBuf,
    file_name: String,
    modified: SystemTime,
    len: u64,
}

fn list_candidates(folders: &[PathBuf], prefix: &str) -> Result<Vec<Candidate>, String> {
    let mut found = Vec::new();
    let mut readable = false;
    let mut last_error = None;
    for folder in folders {
        let entries = match fs::read_dir(folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                last_error = Some(format!("{}: {error}", folder.display()));
                continue;
            }
        };
        readable = true;
        for entry in entries.flatten() {
            let Ok(file_name) = entry.file_name().into_string() else {
                continue;
            };
            if !is_candidate(&file_name, prefix) && file_name != BUGSPLAT_ARCHIVE {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            found.push(Candidate {
                path: entry.path(),
                file_name,
                modified: metadata.modified().unwrap_or(UNIX_EPOCH),
                len: metadata.len(),
            });
        }
    }
    if !readable && let Some(error) = last_error {
        return Err(error);
    }
    found.sort_by_key(|candidate| candidate.modified);
    Ok(found)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LedgerOutcome {
    Accepted,
    Refused,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerEntry {
    pub outcome: LedgerOutcome,
    pub file_name: String,
    pub at_ms: u64,
}

/// Content hashes this companion has finished with, so a restart never resends.
/// A lost or corrupt ledger only costs a resend, which the coordinator deduplicates.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Ledger {
    entries: BTreeMap<String, LedgerEntry>,
}

impl Ledger {
    pub fn load(directory: &Path) -> Self {
        fs::read(directory.join(LEDGER_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, directory: &Path) -> Result<(), String> {
        fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        let staging = directory.join(format!("{LEDGER_FILE}.new"));
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        fs::write(&staging, bytes).map_err(|error| error.to_string())?;
        fs::rename(&staging, directory.join(LEDGER_FILE)).map_err(|error| error.to_string())
    }

    pub fn contains(&self, sha256: &str) -> bool {
        self.entries.contains_key(sha256)
    }

    pub fn record(&mut self, sha256: &str, outcome: LedgerOutcome, file_name: &str, at_ms: u64) {
        self.entries.insert(
            sha256.to_string(),
            LedgerEntry {
                outcome,
                file_name: file_name.to_string(),
                at_ms,
            },
        );
    }

    pub fn accepted(&self) -> impl Iterator<Item = &LedgerEntry> {
        self.entries
            .values()
            .filter(|entry| entry.outcome == LedgerOutcome::Accepted)
    }
}

type FileKey = (PathBuf, SystemTime, u64);

/// One uploadable report: a crash report file, or one entry of Arena's crash zip.
struct Piece {
    file_name: String,
    sha256: String,
    /// The text, or why it cannot be sent.
    content: Result<String, String>,
}

impl Piece {
    fn from_bytes(file_name: String, bytes: Vec<u8>) -> Self {
        let sha256 = sha256_hex(&bytes);
        let content = String::from_utf8(bytes).map_err(|_| "is not text".to_string());
        Self {
            file_name,
            sha256,
            content,
        }
    }
}

/// Owns the ledger and a hash cache, so a file whose reports are all finished
/// is not read again until it changes.
pub struct CrashReporter {
    directory: PathBuf,
    ledger: Ledger,
    hashes: HashMap<FileKey, Vec<String>>,
    status: CrashReportStatus,
}

impl CrashReporter {
    pub fn new(directory: PathBuf) -> Self {
        let ledger = Ledger::load(&directory);
        Self {
            directory,
            ledger,
            hashes: HashMap::new(),
            status: CrashReportStatus::starting(),
        }
    }

    /// Runs one scan and returns the resulting status. `upload` is the network
    /// call, injected so the scan logic is testable without a coordinator.
    pub fn scan(
        &mut self,
        config: &UploadConfig,
        folders: &[PathBuf],
        now_ms: u64,
        upload: &mut dyn FnMut(&Report) -> UploadOutcome,
    ) -> CrashReportStatus {
        let (sent_count, last_sent_at_ms) = self.sent_summary();
        let mut status = CrashReportStatus {
            state: CrashReportState::Watching,
            reason: String::new(),
            sent_count,
            last_sent_at_ms,
            last_error: self.status.last_error.clone(),
        };

        if !config.enabled {
            status.state = CrashReportState::Disabled;
            status.reason = "Crash report upload is turned off.".into();
            return self.finish(status);
        }
        if !cfg!(target_os = "macos") {
            status.state = CrashReportState::Unavailable;
            status.reason = "Crash report upload is not available on this platform.".into();
            return self.finish(status);
        }
        if config.base_url.is_empty() {
            status.state = CrashReportState::NotConfigured;
            status.reason = "Crash reports are not sent because no coordinator address is set. Enter one in settings.".into();
            return self.finish(status);
        }
        if config.token.is_empty() {
            status.state = CrashReportState::NotConfigured;
            status.reason = "Crash reports are not sent because no coordinator token is set. Paste a companion token in settings.".into();
            return self.finish(status);
        }

        let candidates = match list_candidates(folders, &config.executable_prefix) {
            Ok(candidates) => candidates,
            Err(error) => {
                status.state = CrashReportState::FolderUnreadable;
                status.reason = format!(
                    "The crash report folder cannot be read: {error}. Give the companion access to it."
                );
                return self.finish(status);
            }
        };

        let mut attempts = 0;
        let mut stopped: Option<(CrashReportState, String)> = None;
        'candidates: for candidate in candidates {
            if attempts >= UPLOADS_PER_SCAN {
                break;
            }
            if candidate.len > MAX_CONTENT_BYTES {
                status.last_error = Some(format!(
                    "{} is larger than 4 MB and was not sent.",
                    candidate.file_name
                ));
                continue;
            }
            if self.is_finished(&candidate) {
                continue;
            }
            let pieces = match self.read(&candidate) {
                Ok(pieces) => pieces,
                Err(error) => {
                    status.last_error = Some(format!(
                        "{} could not be read: {error}.",
                        candidate.file_name
                    ));
                    continue;
                }
            };
            for piece in pieces {
                if attempts >= UPLOADS_PER_SCAN {
                    break 'candidates;
                }
                if self.ledger.contains(&piece.sha256) {
                    continue;
                }
                let content = match piece.content {
                    Ok(content) => content,
                    Err(why) => {
                        self.ledger.record(
                            &piece.sha256,
                            LedgerOutcome::Refused,
                            &piece.file_name,
                            now_ms,
                        );
                        status.last_error =
                            Some(format!("{} {why} and was not sent.", piece.file_name));
                        continue;
                    }
                };
                attempts += 1;
                let report = Report {
                    file_name: &piece.file_name,
                    captured_at: rfc3339_utc(candidate.modified),
                    sha256: &piece.sha256,
                    content: &content,
                };
                match upload(&report) {
                    UploadOutcome::Accepted => {
                        self.ledger.record(
                            &piece.sha256,
                            LedgerOutcome::Accepted,
                            &piece.file_name,
                            now_ms,
                        );
                        status.last_error = None;
                    }
                    UploadOutcome::Refused(why) => {
                        self.ledger.record(
                            &piece.sha256,
                            LedgerOutcome::Refused,
                            &piece.file_name,
                            now_ms,
                        );
                        status.last_error =
                            Some(format!("{} was not stored because {why}.", piece.file_name));
                    }
                    UploadOutcome::TokenRejected => {
                        stopped = Some((
                            CrashReportState::TokenRejected,
                            "The coordinator rejected the companion token. Paste a new companion token in settings.".into(),
                        ));
                        break 'candidates;
                    }
                    UploadOutcome::Retry(why) => {
                        stopped = Some((
                            CrashReportState::CoordinatorUnreachable,
                            format!(
                                "Cannot send crash reports to the coordinator: {why}. They will be sent when it answers."
                            ),
                        ));
                        break 'candidates;
                    }
                }
            }
        }
        let _ = self.ledger.save(&self.directory);

        let (sent_count, last_sent_at_ms) = self.sent_summary();
        status.sent_count = sent_count;
        status.last_sent_at_ms = last_sent_at_ms;
        if let Some((state, reason)) = stopped {
            status.state = state;
            status.last_error = Some(reason.clone());
            status.reason = reason;
        } else {
            status.reason = match sent_count {
                0 => "Watching for Resolume crash reports. None have been sent.".into(),
                1 => "Watching for Resolume crash reports. 1 has been sent.".into(),
                n => format!("Watching for Resolume crash reports. {n} have been sent."),
            };
        }
        self.finish(status)
    }

    fn finish(&mut self, status: CrashReportStatus) -> CrashReportStatus {
        self.status = status.clone();
        status
    }

    fn sent_summary(&self) -> (usize, Option<u64>) {
        let mut count = 0;
        let mut last = None;
        for entry in self.ledger.accepted() {
            count += 1;
            last = last.max(Some(entry.at_ms));
        }
        (count, last)
    }

    /// True when this unchanged file was read before and every report in it
    /// was accepted or refused.
    fn is_finished(&self, candidate: &Candidate) -> bool {
        self.hashes
            .get(&file_key(candidate))
            .is_some_and(|hashes| hashes.iter().all(|sha256| self.ledger.contains(sha256)))
    }

    /// Reads a crash report file as one report, or Arena's crash zip as one
    /// report per entry.
    fn read(&mut self, candidate: &Candidate) -> Result<Vec<Piece>, String> {
        let bytes = fs::read(&candidate.path).map_err(|error| error.to_string())?;
        let pieces = if candidate.file_name == BUGSPLAT_ARCHIVE {
            archive_pieces(bytes)?
        } else {
            vec![Piece::from_bytes(candidate.file_name.clone(), bytes)]
        };
        self.hashes.insert(
            file_key(candidate),
            pieces.iter().map(|piece| piece.sha256.clone()).collect(),
        );
        Ok(pieces)
    }
}

fn file_key(candidate: &Candidate) -> FileKey {
    (candidate.path.clone(), candidate.modified, candidate.len)
}

/// Splits Arena's crash zip into its files. An entry over 4 MB is read only up
/// to the limit, so a damaged or hostile zip cannot exhaust memory.
fn archive_pieces(bytes: Vec<u8>) -> Result<Vec<Piece>, String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| error.to_string())?;
    let mut pieces = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        if !entry.is_file() {
            continue;
        }
        let Some(file_name) = entry.enclosed_name().and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        }) else {
            continue;
        };
        let mut bytes = Vec::new();
        (&mut entry)
            .take(MAX_CONTENT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_CONTENT_BYTES {
            let sha256 = sha256_hex(&bytes);
            pieces.push(Piece {
                file_name,
                sha256,
                content: Err("is larger than 4 MB".into()),
            });
            continue;
        }
        pieces.push(Piece::from_bytes(file_name, bytes));
    }
    Ok(pieces)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn rfc3339_utc(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let days = (seconds / 86_400) as i64;
    let rem = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// Howard Hinnant's days-to-civil conversion for the proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

pub fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(UPLOAD_TIMEOUT).build()
}

pub fn upload(
    agent: &ureq::Agent,
    base_url: &str,
    token: &str,
    companion_id: &str,
    report: &Report,
) -> UploadOutcome {
    let body = serde_json::json!({
        "companionId": companion_id,
        "kind": REPORT_KIND,
        "source": REPORT_SOURCE,
        "fileName": report.file_name,
        "capturedAt": report.captured_at,
        "contentSha256": report.sha256,
        "content": report.content,
    });
    let result = agent
        .post(&format!("{base_url}{REPORTS_PATH}"))
        .set("Authorization", &format!("Bearer {token}"))
        .send_json(body);
    match result {
        Ok(response) => classify_status(response.status()),
        Err(ureq::Error::Status(code, _)) => classify_status(code),
        Err(ureq::Error::Transport(transport)) => UploadOutcome::Retry(
            transport
                .message()
                .map(str::to_string)
                .unwrap_or_else(|| transport.kind().to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_dir() -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "showmesh-companion-crash-test-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn config() -> UploadConfig {
        UploadConfig {
            enabled: true,
            base_url: "http://coordinator:8080".into(),
            token: "secret".into(),
            companion_id: "resolume-mac".into(),
            executable_prefix: "Arena".into(),
        }
    }

    #[test]
    fn the_scan_filter_takes_only_this_applications_reports() {
        assert!(is_candidate("Arena-2026-10-14-205842.ips", "Arena"));
        assert!(is_candidate("Arena_2026-10-14-205842_iMac.crash", "Arena"));
        assert!(!is_candidate("ArenaHelper-2026-10-14-205842.ips", "Arena"));
        assert!(!is_candidate("Arena-2026-10-14-205842.diag", "Arena"));
        assert!(!is_candidate("Avenue-2026-10-14-205842.ips", "Arena"));
        assert!(!is_candidate("arena-2026-10-14-205842.ips", "Arena"));
    }

    #[test]
    fn the_prefix_comes_from_the_configured_application() {
        assert_eq!(
            executable_prefix("/Applications/Resolume Arena/Arena.app"),
            "Arena"
        );
        assert_eq!(
            executable_prefix("/Applications/Resolume Arena/Arena.app/Contents/MacOS/Arena"),
            "Arena"
        );
        assert_eq!(executable_prefix(""), "Arena");
        assert_eq!(
            executable_prefix(r"C:\Program Files\Resolume Arena\Arena.exe"),
            "Arena"
        );
    }

    #[test]
    fn companion_names_map_onto_the_contract_character_set() {
        assert_eq!(
            normalize_companion_id("ShowMesh Companion"),
            "ShowMesh-Companion"
        );
        assert_eq!(normalize_companion_id("  Booth / Mac #2  "), "Booth-Mac-2");
        assert_eq!(normalize_companion_id("render.node_01"), "render.node_01");
        assert_eq!(normalize_companion_id("###"), "companion");
        assert_eq!(normalize_companion_id(&"x".repeat(80)).len(), 64);
        assert_eq!(normalize_companion_id("Café"), "Caf");
    }

    #[test]
    fn coordinator_answers_are_classified_by_the_contract() {
        assert_eq!(classify_status(201), UploadOutcome::Accepted);
        assert_eq!(classify_status(200), UploadOutcome::Accepted);
        assert!(matches!(classify_status(400), UploadOutcome::Refused(_)));
        assert!(matches!(classify_status(413), UploadOutcome::Refused(_)));
        assert_eq!(classify_status(401), UploadOutcome::TokenRejected);
        assert_eq!(classify_status(403), UploadOutcome::TokenRejected);
        assert!(matches!(classify_status(500), UploadOutcome::Retry(_)));
        assert!(matches!(classify_status(404), UploadOutcome::Retry(_)));
    }

    #[test]
    fn the_ledger_survives_a_restart() {
        let dir = temp_dir();
        let mut ledger = Ledger::default();
        ledger.record("abc", LedgerOutcome::Accepted, "Arena-1.ips", 10);
        ledger.record("def", LedgerOutcome::Refused, "Arena-2.ips", 20);
        ledger.save(&dir).unwrap();

        let loaded = Ledger::load(&dir);
        assert!(loaded.contains("abc"));
        assert!(loaded.contains("def"));
        assert_eq!(loaded.accepted().count(), 1);
    }

    #[test]
    fn a_corrupt_ledger_starts_empty() {
        let dir = temp_dir();
        fs::write(dir.join(LEDGER_FILE), b"not json").unwrap();
        assert!(!Ledger::load(&dir).contains("abc"));
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        let time = UNIX_EPOCH + Duration::from_secs(1_791_154_722);
        assert_eq!(rfc3339_utc(time), "2026-10-04T22:58:42Z");
        assert_eq!(rfc3339_utc(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn the_hash_is_lowercase_hex_sha256() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[cfg(target_os = "macos")]
    mod scanning {
        use super::*;

        #[test]
        fn an_accepted_report_is_sent_once_across_restarts() {
            let state_dir = temp_dir();
            let reports = temp_dir();
            fs::write(
                reports.join("Arena-2026-10-14-205842.ips"),
                b"{\"crash\":1}",
            )
            .unwrap();
            fs::write(reports.join("Wire-2026-10-14-205842.ips"), b"{}").unwrap();

            let mut sent = Vec::new();
            let mut reporter = CrashReporter::new(state_dir.clone());
            let status = reporter.scan(
                &config(),
                std::slice::from_ref(&reports),
                1_000,
                &mut |report| {
                    sent.push(report.file_name.to_string());
                    UploadOutcome::Accepted
                },
            );
            assert_eq!(sent, vec!["Arena-2026-10-14-205842.ips"]);
            assert_eq!(status.state, CrashReportState::Watching);
            assert_eq!(status.sent_count, 1);
            assert_eq!(status.last_sent_at_ms, Some(1_000));

            let mut restarted = CrashReporter::new(state_dir);
            restarted.scan(&config(), &[reports], 2_000, &mut |_| {
                panic!("an accepted report was sent again")
            });
        }

        #[test]
        fn an_unreachable_coordinator_retries_on_the_next_scan() {
            let reports = temp_dir();
            fs::write(reports.join("Arena-1.ips"), b"one").unwrap();
            let mut reporter = CrashReporter::new(temp_dir());

            let status = reporter.scan(&config(), std::slice::from_ref(&reports), 1, &mut |_| {
                UploadOutcome::Retry("connection refused".into())
            });
            assert_eq!(status.state, CrashReportState::CoordinatorUnreachable);
            assert!(status.reason.contains("connection refused"));

            let mut calls = 0;
            let status = reporter.scan(&config(), &[reports], 2, &mut |_| {
                calls += 1;
                UploadOutcome::Accepted
            });
            assert_eq!(calls, 1);
            assert_eq!(status.state, CrashReportState::Watching);
            assert!(status.last_error.is_none());
        }

        #[test]
        fn a_rejected_token_stops_the_scan_and_is_reported() {
            let reports = temp_dir();
            fs::write(reports.join("Arena-1.ips"), b"one").unwrap();
            fs::write(reports.join("Arena-2.ips"), b"two").unwrap();
            let mut calls = 0;
            let status = CrashReporter::new(temp_dir()).scan(&config(), &[reports], 1, &mut |_| {
                calls += 1;
                UploadOutcome::TokenRejected
            });
            assert_eq!(calls, 1);
            assert_eq!(status.state, CrashReportState::TokenRejected);
            assert_eq!(status.sent_count, 0);
        }

        #[test]
        fn a_refused_report_is_not_sent_again() {
            let reports = temp_dir();
            fs::write(reports.join("Arena-1.ips"), b"one").unwrap();
            let mut reporter = CrashReporter::new(temp_dir());
            let status = reporter.scan(&config(), std::slice::from_ref(&reports), 1, &mut |_| {
                UploadOutcome::Refused("the coordinator refused it as invalid".into())
            });
            assert!(status.last_error.unwrap().contains("Arena-1.ips"));
            reporter.scan(&config(), &[reports], 2, &mut |_| {
                panic!("a refused report was sent again")
            });
        }

        #[test]
        fn an_oversized_report_is_skipped_without_reading_it() {
            let reports = temp_dir();
            let file = fs::File::create(reports.join("Arena-big.ips")).unwrap();
            file.set_len(MAX_CONTENT_BYTES + 1).unwrap();
            let status = CrashReporter::new(temp_dir()).scan(&config(), &[reports], 1, &mut |_| {
                panic!("an oversized report was sent")
            });
            assert!(status.last_error.unwrap().contains("larger than 4 MB"));
        }

        #[test]
        fn at_most_a_handful_are_sent_per_scan() {
            let reports = temp_dir();
            for index in 0..8 {
                fs::write(
                    reports.join(format!("Arena-{index}.ips")),
                    format!("{index}"),
                )
                .unwrap();
            }
            let mut calls = 0;
            CrashReporter::new(temp_dir()).scan(&config(), &[reports], 1, &mut |_| {
                calls += 1;
                UploadOutcome::Accepted
            });
            assert_eq!(calls, UPLOADS_PER_SCAN);
        }

        fn write_bugsplat_zip(folder: &Path, entries: &[(&str, &[u8])]) {
            use std::io::Write;
            let file = fs::File::create(folder.join(BUGSPLAT_ARCHIVE)).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            for (name, bytes) in entries {
                writer
                    .start_file(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(bytes).unwrap();
            }
            writer.finish().unwrap();
        }

        #[test]
        fn arenas_crash_zip_is_sent_one_report_per_entry() {
            let state_dir = temp_dir();
            let cache = temp_dir();
            write_bugsplat_zip(
                &cache,
                &[
                    ("lastCrash.log", b"log of the first crash"),
                    ("lastCrash.mem.txt", b"Max MB: 512"),
                    ("lastCrash.additional_recentLayout.xml", b"<layout/>"),
                ],
            );
            let mut sent = Vec::new();
            let mut reporter = CrashReporter::new(state_dir.clone());
            let status = reporter.scan(&config(), std::slice::from_ref(&cache), 1, &mut |report| {
                sent.push((report.file_name.to_string(), report.content.to_string()));
                UploadOutcome::Accepted
            });
            assert_eq!(
                sent,
                vec![
                    (
                        "lastCrash.log".to_string(),
                        "log of the first crash".to_string()
                    ),
                    ("lastCrash.mem.txt".to_string(), "Max MB: 512".to_string()),
                    (
                        "lastCrash.additional_recentLayout.xml".to_string(),
                        "<layout/>".to_string()
                    ),
                ]
            );
            assert_eq!(status.sent_count, 3);

            reporter.scan(&config(), std::slice::from_ref(&cache), 2, &mut |_| {
                panic!("an unchanged crash zip was sent again")
            });

            // The next crash replaces the zip under the same name. Only entries
            // whose content changed are new reports.
            write_bugsplat_zip(
                &cache,
                &[
                    ("lastCrash.log", b"log of the second crash"),
                    ("lastCrash.mem.txt", b"Max MB: 512"),
                    ("lastCrash.additional_recentLayout.xml", b"<layout/>"),
                ],
            );
            let mut sent = Vec::new();
            CrashReporter::new(state_dir).scan(&config(), &[cache], 3, &mut |report| {
                sent.push(report.content.to_string());
                UploadOutcome::Accepted
            });
            assert_eq!(sent, vec!["log of the second crash"]);
        }

        #[test]
        fn a_damaged_crash_zip_is_reported_and_retried() {
            let cache = temp_dir();
            fs::write(cache.join(BUGSPLAT_ARCHIVE), b"not a zip").unwrap();
            let mut reporter = CrashReporter::new(temp_dir());
            let status = reporter.scan(&config(), std::slice::from_ref(&cache), 1, &mut |_| {
                panic!("nothing should be sent from a damaged zip")
            });
            assert_eq!(status.state, CrashReportState::Watching);
            assert!(
                status
                    .last_error
                    .unwrap()
                    .contains("lastCrash.zip could not be read")
            );

            write_bugsplat_zip(&cache, &[("lastCrash.log", b"written in full")]);
            let mut calls = 0;
            reporter.scan(&config(), &[cache], 2, &mut |_| {
                calls += 1;
                UploadOutcome::Accepted
            });
            assert_eq!(calls, 1);
        }

        #[test]
        fn a_crash_zip_entry_that_is_not_text_is_refused_once() {
            let cache = temp_dir();
            write_bugsplat_zip(
                &cache,
                &[
                    ("lastCrash.bin", &[0xff, 0xfe, 0x00]),
                    ("lastCrash.log", b"text"),
                ],
            );
            let mut sent = Vec::new();
            let mut reporter = CrashReporter::new(temp_dir());
            let status = reporter.scan(&config(), std::slice::from_ref(&cache), 1, &mut |report| {
                sent.push(report.file_name.to_string());
                UploadOutcome::Accepted
            });
            assert_eq!(sent, vec!["lastCrash.log"]);
            assert_eq!(status.sent_count, 1);
            reporter.scan(&config(), &[cache], 2, &mut |_| {
                panic!("a finished crash zip was sent again")
            });
        }

        #[test]
        fn missing_settings_are_distinct_states() {
            let mut reporter = CrashReporter::new(temp_dir());
            let mut never = |_: &Report| -> UploadOutcome { panic!("nothing should be sent") };

            let mut disabled = config();
            disabled.enabled = false;
            assert_eq!(
                reporter.scan(&disabled, &[], 1, &mut never).state,
                CrashReportState::Disabled
            );

            let mut no_token = config();
            no_token.token.clear();
            let status = reporter.scan(&no_token, &[], 1, &mut never);
            assert_eq!(status.state, CrashReportState::NotConfigured);
            assert!(status.reason.contains("token"));

            let mut no_address = config();
            no_address.base_url.clear();
            let status = reporter.scan(&no_address, &[], 1, &mut never);
            assert_eq!(status.state, CrashReportState::NotConfigured);
            assert!(status.reason.contains("address"));
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn other_platforms_report_unavailable() {
        let status = CrashReporter::new(temp_dir())
            .scan(&config(), &[], 1, &mut |_| panic!("nothing should be sent"));
        assert_eq!(status.state, CrashReportState::Unavailable);
    }
}

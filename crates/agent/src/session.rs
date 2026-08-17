//! Named, resumable TUI/headless sessions.
//!
//! Each session has its own id (`ridge-YYYYMMDD-xxxxxxxx`). History lives under
//! `~/.ridge/sessions/<id>.json`. `--resume` continues the last session;
//! `--resume <id>` / `--session <id>` continues a specific one.

use provider::Message;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SESSION_SCHEMA: u32 = 1;
const MAX_INDEXED_SESSIONS: usize = 64;

static CURRENT_SESSION_ID: std::sync::OnceLock<std::sync::Mutex<String>> =
    std::sync::OnceLock::new();

pub fn set_current_session_id(id: impl Into<String>) {
    let id = id.into();
    if let Ok(mut slot) = current_slot().lock() {
        *slot = id;
    }
}

pub fn current_session_id() -> String {
    current_slot()
        .lock()
        .map(|id| id.clone())
        .unwrap_or_default()
}

fn current_slot() -> &'static std::sync::Mutex<String> {
    CURRENT_SESSION_ID.get_or_init(|| std::sync::Mutex::new(String::new()))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionRecord {
    pub schema_version: u32,
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub history: Vec<Message>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct SessionIndex {
    last: Option<String>,
    ids: Vec<String>,
}

impl SessionRecord {
    pub fn new(title: impl Into<String>, cwd: impl Into<String>, history: Vec<Message>) -> Self {
        let now = unix_seconds();
        Self {
            schema_version: SESSION_SCHEMA,
            id: new_session_id(now),
            title: title.into(),
            cwd: cwd.into(),
            created_at: now,
            updated_at: now,
            history,
        }
    }
}

pub fn new_session_id(now: u64) -> String {
    let stamp = format_day(now);
    let mix = now
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(std::process::id() as u64);
    format!("ridge-{stamp}-{mix:08x}")
}

pub fn looks_like_session_id(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("ridge-") && value.len() >= 16 && !value.contains(char::is_whitespace)
}

fn ridge_home_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RIDGE_HOME") {
        return PathBuf::from(dir);
    }
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".ridge")
}

pub fn sessions_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RIDGE_SESSIONS_DIR") {
        return PathBuf::from(dir);
    }
    ridge_home_dir().join("sessions")
}

pub fn session_file(id: &str) -> PathBuf {
    sessions_dir().join(format!("{id}.json"))
}

pub fn index_path() -> PathBuf {
    sessions_dir().join("index.json")
}

pub fn save_record(record: &SessionRecord) -> std::io::Result<()> {
    let dir = sessions_dir();
    std::fs::create_dir_all(&dir)?;
    let path = session_file(&record.id);
    let json = serde_json::to_string(record)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    std::fs::write(&path, json)?;
    touch_index(&record.id);
    Ok(())
}

pub fn load_record(id: &str) -> Option<SessionRecord> {
    let text = std::fs::read_to_string(session_file(id)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn list_records() -> Vec<SessionRecord> {
    let mut records: Vec<SessionRecord> = load_index()
        .ids
        .into_iter()
        .filter_map(|id| load_record(&id))
        .collect();
    if records.is_empty() {
        records = scan_session_files();
    }
    records.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
    records
}

pub fn last_session_id() -> Option<String> {
    load_index()
        .last
        .or_else(|| list_records().into_iter().next().map(|record| record.id))
}

pub fn load_history(id: &str) -> Vec<Message> {
    load_record(id)
        .map(|record| record.history)
        .unwrap_or_default()
}

pub fn persist_history(id: &str, history: &[Message], title: Option<&str>, cwd: &Path) {
    let mut record = load_record(id).unwrap_or_else(|| {
        SessionRecord::new(
            title.unwrap_or("session"),
            cwd.display().to_string(),
            Vec::new(),
        )
    });
    record.id = id.to_string();
    record.history = history.to_vec();
    record.updated_at = unix_seconds();
    if record.title.is_empty() || record.title == "session" {
        if let Some(title) = title.filter(|value| !value.trim().is_empty()) {
            record.title = title.to_string();
        } else if let Some(first) = history
            .iter()
            .find(|message| message.role == provider::Role::User)
        {
            record.title = first.content.chars().take(72).collect();
        }
    }
    record.cwd = cwd.display().to_string();
    let _ = save_record(&record);
}

pub fn format_session_list(records: &[SessionRecord]) -> String {
    if records.is_empty() {
        return "no saved sessions".to_string();
    }
    let last = last_session_id();
    records
        .iter()
        .map(|record| {
            let mark = if last.as_deref() == Some(record.id.as_str()) {
                "*"
            } else {
                " "
            };
            format!(
                "{mark} {}  {}  {}",
                record.id,
                record.title.replace('\n', " "),
                record.cwd
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn touch_index(id: &str) {
    let mut index = load_index();
    index.ids.retain(|existing| existing != id);
    index.ids.insert(0, id.to_string());
    index.ids.truncate(MAX_INDEXED_SESSIONS);
    index.last = Some(id.to_string());
    if let Ok(json) = serde_json::to_string(&index) {
        let _ = std::fs::create_dir_all(sessions_dir());
        let _ = std::fs::write(index_path(), json);
    }
}

fn load_index() -> SessionIndex {
    std::fs::read_to_string(index_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn scan_session_files() -> Vec<SessionRecord> {
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let id = name.strip_suffix(".json")?;
            (id != "index").then(|| load_record(id)).flatten()
        })
        .collect()
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn format_day(epoch: u64) -> String {
    const SECS_PER_DAY: u64 = 86_400;
    let days = epoch / SECS_PER_DAY;
    let mut year = 1970u64;
    let mut remaining = days;
    loop {
        let length = if is_leap(year) { 366 } else { 365 };
        if remaining < length {
            break;
        }
        remaining -= length;
        year += 1;
    }
    let month_lens: [u64; 12] = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 1u64;
    for length in month_lens {
        if remaining < length {
            break;
        }
        remaining -= length;
        month += 1;
    }
    let day = remaining + 1;
    format!("{year:04}{month:02}{day:02}")
}

fn is_leap(year: u64) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider::Message;

    #[test]
    fn session_roundtrip_keeps_id_and_history() {
        let root = std::env::temp_dir().join(format!("ridge-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("RIDGE_SESSIONS_DIR", &root);
        let mut record = SessionRecord::new("pack release", "C:\\proj", vec![Message::user("hi")]);
        assert!(looks_like_session_id(&record.id));
        save_record(&record).unwrap();
        persist_history(
            &record.id,
            &[Message::user("hi"), Message::assistant("ok")],
            Some("pack release"),
            Path::new("C:\\proj"),
        );
        let loaded = load_record(&record.id).unwrap();
        assert_eq!(loaded.id, record.id);
        assert_eq!(loaded.history.len(), 2);
        assert_eq!(last_session_id().as_deref(), Some(record.id.as_str()));
        let listed = format_session_list(&list_records());
        assert!(listed.contains(&record.id), "{listed}");
        record.id = "other".into();
        let _ = std::fs::remove_dir_all(&root);
        std::env::remove_var("RIDGE_SESSIONS_DIR");
    }
}

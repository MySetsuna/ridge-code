//! Persistent, deterministic lifecycle for one user goal.
//!
//! This module deliberately stays above the graph/provider layers.  A goal is
//! durable coordination state; model text never decides whether it completed.

use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const GOAL_SCHEMA_VERSION: u32 = 1;
const DEFAULT_GOAL_PATH: &str = ".ridge/goal.json";
const MAX_TEXT_CHARS: usize = 2_000;
const MAX_EVIDENCE: usize = 32;

#[derive(Debug)]
pub enum GoalError {
    NotFound(PathBuf),
    AlreadyExists(PathBuf),
    AlreadyRunning,
    NotRunning,
    InvalidState(String),
    Invalid(String),
    Io(io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for GoalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(path) => write!(formatter, "goal file not found: {}", path.display()),
            Self::AlreadyExists(path) => {
                write!(formatter, "goal already exists: {}", path.display())
            }
            Self::AlreadyRunning => write!(formatter, "goal is already running"),
            Self::NotRunning => write!(formatter, "goal is not running"),
            Self::InvalidState(message) => write!(formatter, "invalid goal state: {message}"),
            Self::Invalid(message) => write!(formatter, "invalid goal input: {message}"),
            Self::Io(error) => write!(formatter, "goal I/O error: {error}"),
            Self::Json(error) => write!(formatter, "invalid goal JSON: {error}"),
        }
    }
}

impl std::error::Error for GoalError {}

impl From<io::Error> for GoalError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for GoalError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Blocked,
    Completed,
    Cancelled,
}

impl GoalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct Goal {
    pub schema_version: u32,
    pub id: String,
    pub title: String,
    pub status: GoalStatus,
    pub phase: String,
    pub evidence: Vec<String>,
    pub failure_reason: Option<String>,
    pub next_step: Option<String>,
    pub running: bool,
    pub revision: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

impl Goal {
    pub fn new(title: &str) -> Result<Self, GoalError> {
        let title = bounded_required(title, "title")?;
        let now = unix_seconds();
        Ok(Self {
            schema_version: GOAL_SCHEMA_VERSION,
            id: format!("goal-{}-{}", now, slugify(&title)),
            title,
            status: GoalStatus::Active,
            phase: "queued".to_string(),
            evidence: Vec::new(),
            failure_reason: None,
            next_step: None,
            running: false,
            revision: 0,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn validate(&self) -> Result<(), GoalError> {
        if self.schema_version != GOAL_SCHEMA_VERSION {
            return Err(GoalError::Invalid(format!(
                "unsupported schema version {}",
                self.schema_version
            )));
        }
        bounded_required(&self.id, "id")?;
        bounded_required(&self.title, "title")?;
        bounded_required(&self.phase, "phase")?;
        if self.evidence.len() > MAX_EVIDENCE {
            return Err(GoalError::Invalid(format!(
                "evidence exceeds {MAX_EVIDENCE} entries"
            )));
        }
        for item in &self.evidence {
            bounded_required(item, "evidence")?;
        }
        if let Some(reason) = &self.failure_reason {
            bounded_required(reason, "failure_reason")?;
        }
        if let Some(next) = &self.next_step {
            bounded_required(next, "next_step")?;
        }
        if self.running && self.status != GoalStatus::Active {
            return Err(GoalError::InvalidState(
                "only an active goal may be running".to_string(),
            ));
        }
        if matches!(
            self.status,
            GoalStatus::Blocked | GoalStatus::Completed | GoalStatus::Cancelled
        ) && self.running
        {
            return Err(GoalError::InvalidState(
                "terminal or blocked goal cannot be running".to_string(),
            ));
        }
        Ok(())
    }

    pub fn start(&mut self) -> Result<(), GoalError> {
        self.require_active()?;
        if self.running {
            return Err(GoalError::AlreadyRunning);
        }
        if self.phase == "queued" {
            self.phase = "running".to_string();
        }
        self.running = true;
        self.touch();
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), GoalError> {
        if !self.running {
            return Err(GoalError::NotRunning);
        }
        self.running = false;
        if self.phase == "running" {
            self.phase = "paused".to_string();
        }
        self.touch();
        Ok(())
    }

    pub fn advance(
        &mut self,
        phase: &str,
        evidence: &str,
        next_step: Option<&str>,
    ) -> Result<(), GoalError> {
        self.require_active()?;
        self.phase = bounded_required(phase, "phase")?;
        self.append_evidence(evidence)?;
        self.next_step = bounded_optional(next_step, "next_step")?;
        self.running = true;
        self.touch();
        Ok(())
    }

    pub fn complete(&mut self, evidence: &str) -> Result<(), GoalError> {
        self.require_active()?;
        self.append_evidence(evidence)?;
        self.status = GoalStatus::Completed;
        self.phase = "completed".to_string();
        self.failure_reason = None;
        self.next_step = None;
        self.running = false;
        self.touch();
        Ok(())
    }

    pub fn block(&mut self, reason: &str, next_step: Option<&str>) -> Result<(), GoalError> {
        self.require_active()?;
        self.failure_reason = Some(bounded_required(reason, "failure_reason")?);
        self.next_step = bounded_optional(next_step, "next_step")?;
        self.status = GoalStatus::Blocked;
        self.phase = "blocked".to_string();
        self.running = false;
        self.touch();
        Ok(())
    }

    pub fn resume(&mut self) -> Result<(), GoalError> {
        match self.status {
            GoalStatus::Blocked | GoalStatus::Cancelled => {
                self.status = GoalStatus::Active;
                self.phase = "resumed".to_string();
                self.failure_reason = None;
                self.running = false;
                self.touch();
                Ok(())
            }
            GoalStatus::Active => Err(GoalError::InvalidState(
                "goal is already active; use start after stop".to_string(),
            )),
            GoalStatus::Completed => Err(GoalError::InvalidState(
                "completed goal cannot resume".to_string(),
            )),
        }
    }

    pub fn cancel(&mut self, reason: Option<&str>) -> Result<(), GoalError> {
        if self.status == GoalStatus::Completed {
            return Err(GoalError::InvalidState(
                "completed goal cannot be cancelled".to_string(),
            ));
        }
        if let Some(reason) = reason {
            self.failure_reason = Some(bounded_required(reason, "failure_reason")?);
        }
        self.status = GoalStatus::Cancelled;
        self.phase = "cancelled".to_string();
        self.next_step = None;
        self.running = false;
        self.touch();
        Ok(())
    }

    fn require_active(&self) -> Result<(), GoalError> {
        if self.status == GoalStatus::Active {
            Ok(())
        } else {
            Err(GoalError::InvalidState(format!(
                "goal is {}, resume it before changing active work",
                self.status.as_str()
            )))
        }
    }

    fn append_evidence(&mut self, evidence: &str) -> Result<(), GoalError> {
        let evidence = bounded_required(evidence, "evidence")?;
        if self.evidence.len() == MAX_EVIDENCE {
            self.evidence.remove(0);
        }
        self.evidence.push(evidence);
        Ok(())
    }

    fn touch(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.updated_at = unix_seconds();
    }
}

pub fn goal_path() -> PathBuf {
    std::env::var_os("RIDGE_GOAL_FILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_GOAL_PATH))
}

pub fn load_goal(path: impl AsRef<Path>) -> Result<Goal, GoalError> {
    let path = path.as_ref();
    let text = fs::read_to_string(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            GoalError::NotFound(path.to_path_buf())
        } else {
            GoalError::Io(error)
        }
    })?;
    let goal: Goal = serde_json::from_str(&text)?;
    goal.validate()?;
    Ok(goal)
}

pub fn save_goal(path: impl AsRef<Path>, goal: &Goal) -> Result<(), GoalError> {
    let path = path.as_ref();
    goal.validate()?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.as_os_str().is_empty() {
        fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("goal.json");
    let temp = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        unix_millis()
    ));
    let result: Result<(), GoalError> = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        let body = serde_json::to_vec_pretty(goal)?;
        file.write_all(&body)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        replace_file(&temp, path).map_err(GoalError::Io)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn create_goal(path: impl AsRef<Path>, title: &str) -> Result<Goal, GoalError> {
    let path = path.as_ref();
    if path.exists() {
        return Err(GoalError::AlreadyExists(path.to_path_buf()));
    }
    let goal = Goal::new(title)?;
    save_goal(path, &goal)?;
    Ok(goal)
}

/// Create a goal from an interactive command and put it into the running
/// state in one durable write sequence.
pub fn create_and_start_goal(path: impl AsRef<Path>, title: &str) -> Result<Goal, GoalError> {
    let path = path.as_ref();
    if path.exists() {
        return Err(GoalError::AlreadyExists(path.to_path_buf()));
    }
    let mut goal = Goal::new(title)?;
    goal.start()?;
    goal.advance(
        "running",
        "goal set from interactive user input",
        Some("execute and verify"),
    )?;
    save_goal(path, &goal)?;
    Ok(goal)
}

pub fn update_goal<F>(path: impl AsRef<Path>, update: F) -> Result<Goal, GoalError>
where
    F: FnOnce(&mut Goal) -> Result<(), GoalError>,
{
    let path = path.as_ref();
    let mut goal = load_goal(path)?;
    update(&mut goal)?;
    save_goal(path, &goal)?;
    Ok(goal)
}

pub fn render_goal(goal: &Goal) -> String {
    let evidence = if goal.evidence.is_empty() {
        "(none)".to_string()
    } else {
        goal.evidence
            .iter()
            .enumerate()
            .map(|(index, item)| format!("{}. {item}", index + 1))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "goal {}\nstatus: {}\nphase: {}\nrunning: {}\nrevision: {}\nevidence:\n{}\nfailure: {}\nnext: {}",
        goal.id,
        goal.status.as_str(),
        goal.phase,
        goal.running,
        goal.revision,
        evidence,
        goal.failure_reason.as_deref().unwrap_or("(none)"),
        goal.next_step.as_deref().unwrap_or("(none)")
    )
}

pub fn goal_command(args: &[String]) -> Result<String, GoalError> {
    goal_command_at(goal_path(), args)
}

pub fn goal_command_at(path: impl AsRef<Path>, args: &[String]) -> Result<String, GoalError> {
    let path = path.as_ref();
    let command = args.first().map(String::as_str).unwrap_or("status");
    match command {
        "help" => Ok(goal_usage().to_string()),
        "create" => {
            let title = join_required(&args[1..], "title")?;
            Ok(render_goal(&create_goal(path, &title)?))
        }
        "status" | "show" => match load_goal(path) {
            Ok(goal) => Ok(render_goal(&goal)),
            Err(GoalError::NotFound(_)) => Ok(format!("no goal at {}", path.display())),
            Err(error) => Err(error),
        },
        "start" => Ok(render_goal(&update_goal(path, |goal| goal.start())?)),
        "stop" => Ok(render_goal(&update_goal(path, |goal| goal.stop())?)),
        "resume" | "continue" => Ok(render_goal(&update_goal(path, |goal| goal.resume())?)),
        "advance" => {
            let (values, next_step) = parse_tail(&args[1..])?;
            if values.len() < 2 {
                return Err(GoalError::Invalid(
                    "advance requires <phase> <evidence>".to_string(),
                ));
            }
            let phase = values[0].clone();
            let evidence = values[1..].join(" ");
            Ok(render_goal(&update_goal(path, |goal| {
                goal.advance(&phase, &evidence, next_step.as_deref())
            })?))
        }
        "complete" => {
            let evidence = join_required(&args[1..], "evidence")?;
            Ok(render_goal(&update_goal(path, |goal| {
                goal.complete(&evidence)
            })?))
        }
        "block" => {
            let (values, next_step) = parse_tail(&args[1..])?;
            let reason = values.join(" ");
            if reason.is_empty() {
                return Err(GoalError::Invalid("block requires <reason>".to_string()));
            }
            Ok(render_goal(&update_goal(path, |goal| {
                goal.block(&reason, next_step.as_deref())
            })?))
        }
        "cancel" => {
            let reason = args[1..].iter().map(String::as_str).collect::<Vec<_>>();
            let reason = if reason.is_empty() {
                None
            } else {
                Some(reason.join(" "))
            };
            Ok(render_goal(&update_goal(path, |goal| {
                goal.cancel(reason.as_deref())
            })?))
        }
        other => Err(GoalError::Invalid(format!(
            "unknown goal command {other};\n{}",
            goal_usage()
        ))),
    }
}

/// Parse TUI `/goal` text. A non-lifecycle first word is shorthand for
/// `create`, so `/goal 'fix the parser'` and `/goal fix the parser` agree.
pub fn parse_goal_text(text: &str) -> Result<Vec<String>, GoalError> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut token_started = false;
    for character in text.trim().chars() {
        parse_goal_character(
            character,
            &mut current,
            &mut quote,
            &mut escaped,
            &mut token_started,
            &mut words,
        );
    }
    if escaped {
        current.push('\\');
        token_started = true;
    }
    if quote.is_some() {
        return Err(GoalError::Invalid(
            "unterminated quote in goal title".to_string(),
        ));
    }
    if token_started {
        words.push(current);
    }
    Ok(normalize_goal_args(&words))
}

fn parse_goal_character(
    character: char,
    current: &mut String,
    quote: &mut Option<char>,
    escaped: &mut bool,
    token_started: &mut bool,
    words: &mut Vec<String>,
) {
    if *escaped {
        if !matches!(character, '\\' | '\'' | '"') && !character.is_whitespace() {
            current.push('\\');
        }
        current.push(character);
        *token_started = true;
        *escaped = false;
        return;
    }
    if character == '\\' && *quote != Some('\'') {
        *escaped = true;
        *token_started = true;
        return;
    }
    if let Some(open) = *quote {
        if character == open {
            *quote = None;
        } else {
            current.push(character);
        }
        *token_started = true;
        return;
    }
    if matches!(character, '\'' | '"') {
        *quote = Some(character);
        *token_started = true;
    } else if character.is_whitespace() {
        if *token_started {
            words.push(std::mem::take(current));
            *token_started = false;
        }
    } else {
        current.push(character);
        *token_started = true;
    }
}

fn normalize_goal_args(args: &[String]) -> Vec<String> {
    let Some(first) = args.first() else {
        return vec!["status".to_string()];
    };
    if is_goal_command(first) {
        return args.to_vec();
    }
    let mut normalized = Vec::with_capacity(args.len() + 1);
    normalized.push("create".to_string());
    normalized.extend(args.iter().cloned());
    normalized
}

fn is_goal_command(command: &str) -> bool {
    matches!(
        command,
        "help"
            | "status"
            | "show"
            | "create"
            | "start"
            | "stop"
            | "resume"
            | "continue"
            | "advance"
            | "complete"
            | "block"
            | "cancel"
    )
}

fn parse_tail(args: &[String]) -> Result<(Vec<String>, Option<String>), GoalError> {
    let mut values = Vec::new();
    let mut next_step = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--next" {
            if index + 1 >= args.len() {
                return Err(GoalError::Invalid("--next requires text".to_string()));
            }
            next_step = Some(args[index + 1].clone());
            index += 2;
        } else {
            values.push(args[index].clone());
            index += 1;
        }
    }
    Ok((values, next_step))
}

fn join_required(args: &[String], label: &str) -> Result<String, GoalError> {
    let value = args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    bounded_required(&value, label)
}

fn bounded_required(value: &str, label: &str) -> Result<String, GoalError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(GoalError::Invalid(format!("{label} cannot be empty")));
    }
    if value.chars().count() > MAX_TEXT_CHARS {
        return Err(GoalError::Invalid(format!(
            "{label} exceeds {MAX_TEXT_CHARS} characters"
        )));
    }
    Ok(value.to_string())
}

fn bounded_optional(value: Option<&str>, label: &str) -> Result<Option<String>, GoalError> {
    value
        .map(|value| bounded_required(value, label))
        .transpose()
}

fn slugify(value: &str) -> String {
    let slug = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "goal".to_string()
    } else {
        slug.chars().take(48).collect()
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

fn goal_usage() -> &'static str {
    "ridgecode goal [status|create <title>|start|stop|advance <phase> <evidence> [--next <step>]|resume|complete <evidence>|block <reason> [--next <step>]|cancel [reason]]; run with `ridgecode goal run`; TUI shorthand: /goal 'title'"
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }

    let from = wide(from);
    let to = wide(to);
    let flags = 0x0000_0001 | 0x0000_0008;
    let result = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ridge-goal-{name}-{}-{}.json",
            std::process::id(),
            unix_millis()
        ))
    }

    #[test]
    fn lifecycle_rejects_duplicate_start_and_requires_evidence() {
        let mut goal = Goal::new("prove long task convergence").unwrap();
        assert!(goal.complete("").is_err());
        goal.start().unwrap();
        assert_eq!(
            goal.start().unwrap_err().to_string(),
            "goal is already running"
        );
        goal.advance("verify", "quality gate passed", Some("run PTY smoke"))
            .unwrap();
        goal.block("PTY unavailable", Some("install a Windows PTY harness"))
            .unwrap();
        assert_eq!(goal.status, GoalStatus::Blocked);
        goal.resume().unwrap();
        goal.complete("PTY smoke passed").unwrap();
        assert_eq!(goal.status, GoalStatus::Completed);
        assert!(goal.start().is_err());
    }

    #[test]
    fn atomic_save_roundtrips_after_restart_without_temp_leftovers() {
        let path = temp_path("atomic");
        let goal = Goal::new("round trip").unwrap();
        save_goal(&path, &goal).unwrap();
        let loaded = load_goal(&path).unwrap();
        assert_eq!(loaded, goal);
        let prefix = format!(".{}.tmp-", path.file_name().unwrap().to_string_lossy());
        let leftovers = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix));
        assert!(!leftovers);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn command_roundtrip_persists_status_and_resume() {
        let path = temp_path("commands");
        let args = |items: &[&str]| -> Vec<String> {
            items.iter().map(|item| (*item).to_string()).collect()
        };
        goal_command_at(&path, &args(&["create", "ship", "stable", "release"])).unwrap();
        goal_command_at(
            &path,
            &args(&["advance", "verify", "tests-passed", "--next", "run-pty"]),
        )
        .unwrap();
        goal_command_at(&path, &args(&["block", "waiting-on-pty"])).unwrap();
        assert_eq!(load_goal(&path).unwrap().status, GoalStatus::Blocked);
        goal_command_at(&path, &args(&["resume"])).unwrap();
        assert_eq!(load_goal(&path).unwrap().status, GoalStatus::Active);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn argument_helpers_reject_empty_and_bound_input() {
        let args = vec![
            "first".to_string(),
            "--next".to_string(),
            "follow".to_string(),
        ];
        assert_eq!(
            parse_tail(&args).unwrap(),
            (vec!["first".into()], Some("follow".into()))
        );
        assert!(parse_tail(&["--next".into()]).is_err());
        assert_eq!(
            join_required(&[" ship ".into(), "it ".into()], "title").unwrap(),
            "ship  it"
        );
        assert!(bounded_required(" ", "title").is_err());
        assert!(bounded_required(&"x".repeat(MAX_TEXT_CHARS + 1), "title").is_err());
        assert_eq!(bounded_optional(None, "evidence").unwrap(), None);
        assert!(bounded_optional(Some(" "), "evidence").is_err());
        assert_eq!(slugify("  Hello, RidgeCode!  "), "hello--ridgecode");
        assert_eq!(slugify("!!!"), "goal");
        assert!(goal_usage().contains("ridgecode goal"));
    }

    #[test]
    fn shorthand_goal_text_preserves_the_full_quoted_multi_word_payload() {
        assert_eq!(
            parse_goal_text("'修复终端输入并保留历史'").unwrap(),
            vec!["create", "修复终端输入并保留历史"]
        );
        assert_eq!(
            parse_goal_text("\"ship   stable release\"").unwrap(),
            vec!["create", "ship   stable release"]
        );
        assert_eq!(
            parse_goal_text("create \"ship stable\"").unwrap(),
            vec!["create", "ship stable"]
        );
        assert_eq!(parse_goal_text("status").unwrap(), vec!["status"]);
        assert!(parse_goal_text("'unterminated").is_err());
        assert!(parse_goal_text("''").is_ok_and(|args| { args == vec!["create", ""] }));

        let path = temp_path("shorthand");
        goal_command_at(&path, &parse_goal_text("'ship stable release'").unwrap()).unwrap();
        assert_eq!(load_goal(&path).unwrap().title, "ship stable release");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn interactive_goal_creation_starts_and_persists_running_state() {
        let path = temp_path("interactive-start");
        let goal = create_and_start_goal(&path, "ship from tui").unwrap();
        assert_eq!(goal.phase, "running");
        assert!(goal.running);
        assert_eq!(load_goal(&path).unwrap().title, "ship from tui");
        assert!(load_goal(&path).unwrap().running);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn goal_validation_and_terminal_transitions_keep_state_consistent() {
        assert_eq!(GoalStatus::Active.as_str(), "active");
        assert_eq!(GoalStatus::Blocked.as_str(), "blocked");
        assert_eq!(GoalStatus::Completed.as_str(), "completed");
        assert_eq!(GoalStatus::Cancelled.as_str(), "cancelled");

        let mut goal = Goal::new("state checks").unwrap();
        assert!(goal.stop().is_err());
        goal.start().unwrap();
        goal.stop().unwrap();
        assert_eq!(goal.phase, "paused");
        goal.cancel(Some("user stopped")).unwrap();
        assert_eq!(goal.status, GoalStatus::Cancelled);
        assert!(goal.resume().is_ok());
        assert!(goal.resume().is_err());
        goal.complete("done").unwrap();
        assert!(goal.cancel(None).is_err());

        let mut invalid = Goal::new("invalid").unwrap();
        invalid.schema_version = 99;
        assert!(invalid.validate().is_err());
        invalid.schema_version = GOAL_SCHEMA_VERSION;
        invalid.evidence = vec!["e".into(); MAX_EVIDENCE + 1];
        assert!(invalid.validate().is_err());
        invalid.evidence.clear();
        invalid.running = true;
        invalid.status = GoalStatus::Blocked;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn goal_commands_cover_status_lifecycle_and_errors() {
        let path = temp_path("all-commands");
        let args = |items: &[&str]| {
            items
                .iter()
                .map(|item| (*item).to_string())
                .collect::<Vec<_>>()
        };
        assert!(goal_command_at(&path, &args(&["help"]))
            .unwrap()
            .contains("goal ["));
        assert!(goal_command_at(&path, &args(&["status"]))
            .unwrap()
            .contains("no goal at"));
        goal_command_at(&path, &args(&["create", "quality", "gate"])).unwrap();
        assert!(goal_command_at(&path, &args(&["create", "again"])).is_err());
        goal_command_at(&path, &args(&["start"])).unwrap();
        goal_command_at(&path, &args(&["stop"])).unwrap();
        goal_command_at(
            &path,
            &args(&["advance", "tests", "pass", "--next", "coverage"]),
        )
        .unwrap();
        goal_command_at(&path, &args(&["complete", "all", "green"])).unwrap();
        assert!(goal_command_at(&path, &args(&["show"]))
            .unwrap()
            .contains("completed"));
        assert!(goal_command_at(&path, &args(&["unknown"])).is_err());

        let blocked = temp_path("block-command");
        goal_command_at(&blocked, &args(&["create", "blocked"])).unwrap();
        goal_command_at(&blocked, &args(&["block", "needs", "review"])).unwrap();
        assert!(goal_command_at(&blocked, &args(&["cancel"])).is_ok());
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(blocked);
    }

    #[test]
    fn goal_storage_and_rendering_report_invalid_or_optional_fields() {
        let missing = temp_path("missing");
        assert!(matches!(load_goal(&missing), Err(GoalError::NotFound(_))));
        fs::write(&missing, "not json").unwrap();
        assert!(matches!(load_goal(&missing), Err(GoalError::Json(_))));

        let mut goal = Goal::new("render").unwrap();
        assert!(goal.advance(" ", "evidence", None).is_err());
        assert!(goal.complete("").is_err());
        assert!(goal.block("", None).is_err());
        goal.failure_reason = Some("blocked by review".into());
        goal.next_step = Some("run quality gate".into());
        goal.evidence.push("baseline captured".into());
        let rendered = render_goal(&goal);
        assert!(rendered.contains("1. baseline captured"));
        assert!(rendered.contains("blocked by review"));
        assert!(rendered.contains("run quality gate"));
        let _ = fs::remove_file(missing);
    }
}

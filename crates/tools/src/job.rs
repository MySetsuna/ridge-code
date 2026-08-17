//! Bounded long-running shell jobs.
//!
//! The default 180s slice no longer kills the process tree. The command is
//! parked, the observation carries a `job_id`, and a later poll returns the
//! real finish/fail/still-running state. A separate hard timeout still kills.

use super::{
    configure_process_group, decode_bytes, shell_command, terminate_process_tree, ShellResult,
};
use std::collections::HashMap;
use std::io::{self, Read};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_SHELL_HARD_TIMEOUT_SECS: u64 = 1800;
const MAX_LIVE_JOBS: usize = 4;
const OUTPUT_CAP: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct JobProgress {
    pub id: String,
    pub cmd: String,
    pub elapsed_ms: u64,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub enum ShellObservation {
    Finished(ShellResult),
    Running(JobProgress),
}

struct LiveJob {
    id: String,
    cmd: String,
    started: Instant,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    done: Arc<Mutex<Option<ShellResult>>>,
    cancel: Arc<AtomicBool>,
}

fn registry() -> &'static Mutex<HashMap<String, LiveJob>> {
    static JOBS: OnceLock<Mutex<HashMap<String, LiveJob>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn new_job_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    format!(
        "sh-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn shell_slice_timeout() -> Duration {
    env_secs("RIDGE_SHELL_TIMEOUT").unwrap_or(Duration::from_secs(180))
}

pub fn shell_hard_timeout() -> Duration {
    env_secs("RIDGE_SHELL_HARD_TIMEOUT")
        .unwrap_or(Duration::from_secs(DEFAULT_SHELL_HARD_TIMEOUT_SECS))
}

fn env_secs(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
}

pub fn has_live_shell_jobs() -> bool {
    let Ok(jobs) = registry().lock() else {
        return false;
    };
    jobs.values()
        .any(|job| job.done.lock().map(|done| done.is_none()).unwrap_or(false))
}

pub fn live_shell_job_ids() -> Vec<String> {
    let Ok(jobs) = registry().lock() else {
        return Vec::new();
    };
    jobs.iter()
        .filter(|(_, job)| job.done.lock().map(|done| done.is_none()).unwrap_or(false))
        .map(|(id, _)| id.clone())
        .collect()
}

pub fn run_or_park_shell(shell: Option<&str>, cmd: &str) -> io::Result<ShellObservation> {
    run_or_park_shell_with_limits(shell, cmd, shell_slice_timeout(), shell_hard_timeout())
}

pub fn run_or_park_shell_with_limits(
    shell: Option<&str>,
    cmd: &str,
    slice: Duration,
    hard: Duration,
) -> io::Result<ShellObservation> {
    if let Some(existing) = snapshot_matching_cmd(cmd) {
        return Ok(existing);
    }
    if live_count() >= MAX_LIVE_JOBS {
        return Ok(ShellObservation::Finished(ShellResult {
            code: -1,
            stdout: String::new(),
            stderr: format!("too many live shell jobs ({MAX_LIVE_JOBS}); poll job_id first"),
        }));
    }

    let mut command = shell_command(shell, cmd);
    configure_process_group(&mut command);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("shell stdout pipe unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("shell stderr pipe unavailable"))?;
    let stdout_buf = Arc::new(Mutex::new(Vec::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::new()));
    spawn_reader(stdout, stdout_buf.clone());
    spawn_reader(stderr, stderr_buf.clone());
    let done = Arc::new(Mutex::new(None));
    let waiter_done = done.clone();
    let waiter_stdout = stdout_buf.clone();
    let waiter_stderr = stderr_buf.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let waiter_cancel = cancel.clone();
    let started = Instant::now();
    let hard_deadline = started + hard;
    thread::spawn(move || {
        let result = wait_child(
            child,
            waiter_stdout,
            waiter_stderr,
            hard_deadline,
            waiter_cancel,
        );
        if let Ok(mut slot) = waiter_done.lock() {
            *slot = Some(result);
        }
    });

    let slice_deadline = Instant::now() + slice;
    loop {
        if let Some(result) = peek_done(&done) {
            return Ok(ShellObservation::Finished(result));
        }
        if Instant::now() >= slice_deadline {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    let id = new_job_id();
    let job = LiveJob {
        id: id.clone(),
        cmd: cmd.to_string(),
        started,
        stdout: stdout_buf,
        stderr: stderr_buf,
        done,
        cancel,
    };
    let progress = job.progress();
    if let Ok(mut jobs) = registry().lock() {
        jobs.insert(id, job);
    }
    Ok(ShellObservation::Running(progress))
}

pub fn poll_shell_job(id: &str) -> io::Result<ShellObservation> {
    let Ok(mut jobs) = registry().lock() else {
        return Ok(ShellObservation::Finished(ShellResult {
            code: -1,
            stdout: String::new(),
            stderr: format!("job lock poisoned: {id}"),
        }));
    };
    let Some(job) = jobs.get(id) else {
        return Ok(ShellObservation::Finished(ShellResult {
            code: -1,
            stdout: String::new(),
            stderr: format!("unknown job {id}"),
        }));
    };
    if let Some(result) = peek_done(&job.done) {
        jobs.remove(id);
        return Ok(ShellObservation::Finished(result));
    }
    Ok(ShellObservation::Running(job.progress()))
}

/// Request cancellation of one parked job and wait a bounded interval for its
/// process tree to settle. The job remains pollable if host termination takes
/// longer than the local cancellation slice.
pub fn cancel_shell_job(id: &str) -> io::Result<ShellObservation> {
    {
        let jobs = registry()
            .lock()
            .map_err(|_| io::Error::other(format!("job lock poisoned: {id}")))?;
        let Some(job) = jobs.get(id) else {
            return Ok(unknown_job(id));
        };
        job.cancel.store(true, Ordering::Release);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let observation = poll_shell_job(id)?;
        if matches!(observation, ShellObservation::Finished(_)) || Instant::now() >= deadline {
            return Ok(observation);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn unknown_job(id: &str) -> ShellObservation {
    ShellObservation::Finished(ShellResult {
        code: -1,
        stdout: String::new(),
        stderr: format!(
            "unknown job {id}; it may belong to a previous process, restart the command if safe"
        ),
    })
}

fn live_count() -> usize {
    let Ok(jobs) = registry().lock() else {
        return 0;
    };
    jobs.values()
        .filter(|job| job.done.lock().map(|done| done.is_none()).unwrap_or(false))
        .count()
}

fn snapshot_matching_cmd(cmd: &str) -> Option<ShellObservation> {
    let jobs = registry().lock().ok()?;
    jobs.values()
        .find(|job| job.cmd == cmd && job.done.lock().map(|done| done.is_none()).unwrap_or(false))
        .map(|job| ShellObservation::Running(job.progress()))
}

fn peek_done(done: &Arc<Mutex<Option<ShellResult>>>) -> Option<ShellResult> {
    done.lock().ok().and_then(|slot| slot.clone())
}

impl LiveJob {
    fn progress(&self) -> JobProgress {
        JobProgress {
            id: self.id.clone(),
            cmd: self.cmd.clone(),
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            stdout: decode_bytes(&lock_copy(&self.stdout)),
            stderr: decode_bytes(&lock_copy(&self.stderr)),
        }
    }
}

fn lock_copy(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    buf.lock().map(|bytes| bytes.clone()).unwrap_or_default()
}

fn spawn_reader(mut pipe: impl Read + Send + 'static, buf: Arc<Mutex<Vec<u8>>>) {
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut bytes) = buf.lock() {
                        if bytes.len() < OUTPUT_CAP {
                            let room = OUTPUT_CAP - bytes.len();
                            bytes.extend_from_slice(&chunk[..n.min(room)]);
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });
}

fn wait_child(
    mut child: std::process::Child,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    hard_deadline: Instant,
    cancel: Arc<AtomicBool>,
) -> ShellResult {
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if cancel.load(Ordering::Acquire) => {
                terminate_process_tree(&mut child);
                let _ = child.wait();
                let mut err = lock_copy(&stderr);
                err.extend_from_slice(b"\ncommand cancelled (job killed)");
                return ShellResult {
                    code: -1,
                    stdout: decode_bytes(&lock_copy(&stdout)),
                    stderr: decode_bytes(&err),
                };
            }
            Ok(None) if Instant::now() >= hard_deadline => {
                terminate_process_tree(&mut child);
                let _ = child.wait();
                let mut err = lock_copy(&stderr);
                err.extend_from_slice(b"\ncommand hit hard timeout (job killed)");
                return ShellResult {
                    code: -1,
                    stdout: decode_bytes(&lock_copy(&stdout)),
                    stderr: decode_bytes(&err),
                };
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                return ShellResult {
                    code: -1,
                    stdout: decode_bytes(&lock_copy(&stdout)),
                    stderr: decode_bytes(&lock_copy(&stderr)),
                };
            }
        }
    };
    ShellResult {
        code: status.code().unwrap_or(-1),
        stdout: decode_bytes(&lock_copy(&stdout)),
        stderr: decode_bytes(&lock_copy(&stderr)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn long_command_parks_then_polls_to_completion() {
        let slice = Duration::from_millis(200);
        let hard = Duration::from_secs(10);
        #[cfg(windows)]
        let cmd = "Start-Sleep -Seconds 1; Write-Output parked-ok";
        #[cfg(not(windows))]
        let cmd = "sleep 1; echo parked-ok";
        #[cfg(windows)]
        let shell = Some("powershell");
        #[cfg(not(windows))]
        let shell = Some("sh");

        let first = run_or_park_shell_with_limits(shell, cmd, slice, hard).unwrap();
        let job_id = match first {
            ShellObservation::Running(progress) => {
                assert!(!progress.id.is_empty());
                assert!(
                    !progress.stderr.contains("command timed out after 180000ms"),
                    "{}",
                    progress.stderr
                );
                progress.id
            }
            ShellObservation::Finished(result) => {
                assert_eq!(result.code, 0, "{}{}", result.stdout, result.stderr);
                assert!(result.stdout.contains("parked-ok"), "{}", result.stdout);
                return;
            }
        };

        let started = Instant::now();
        let final_obs = loop {
            match poll_shell_job(&job_id).unwrap() {
                ShellObservation::Finished(result) => break result,
                ShellObservation::Running(_) if started.elapsed() < Duration::from_secs(8) => {
                    thread::sleep(Duration::from_millis(50));
                }
                ShellObservation::Running(progress) => {
                    panic!("job still running after 8s: {progress:?}");
                }
            }
        };
        assert_eq!(
            final_obs.code, 0,
            "{}{}",
            final_obs.stdout, final_obs.stderr
        );
        assert!(
            final_obs.stdout.contains("parked-ok"),
            "{}",
            final_obs.stdout
        );
        assert!(
            !final_obs
                .stderr
                .contains("command timed out after 180000ms"),
            "{}",
            final_obs.stderr
        );
    }

    #[test]
    #[ignore = "wall-clock evidence: default 180s slice"]
    fn default_180s_slice_parks_without_taskkill() {
        #[cfg(windows)]
        let first = run_or_park_shell(
            Some("powershell"),
            "Start-Sleep -Seconds 185; Write-Output long-ok",
        )
        .unwrap();
        #[cfg(not(windows))]
        let first = run_or_park_shell(Some("sh"), "sleep 185; echo long-ok").unwrap();
        let id = match first {
            ShellObservation::Running(progress) => {
                assert!(
                    !progress.stderr.contains("command timed out after 180000ms"),
                    "{}",
                    progress.stderr
                );
                progress.id
            }
            ShellObservation::Finished(result) => {
                assert_eq!(result.code, 0, "{}{}", result.stdout, result.stderr);
                assert!(
                    !result.stderr.contains("command timed out after 180000ms"),
                    "{}",
                    result.stderr
                );
                return;
            }
        };
        let started = Instant::now();
        let result = loop {
            match poll_shell_job(&id).unwrap() {
                ShellObservation::Finished(result) => break result,
                ShellObservation::Running(_) if started.elapsed() < Duration::from_secs(30) => {
                    thread::sleep(Duration::from_millis(200));
                }
                ShellObservation::Running(progress) => {
                    panic!("still running after extra 30s: {progress:?}");
                }
            }
        };
        assert_eq!(result.code, 0, "{}{}", result.stdout, result.stderr);
        assert!(result.stdout.contains("long-ok"), "{}", result.stdout);
        assert!(
            !result.stderr.contains("command timed out after 180000ms"),
            "{}",
            result.stderr
        );
    }

    #[test]
    fn hard_timeout_still_kills() {
        let slice = Duration::from_millis(30);
        let hard = Duration::from_millis(120);
        #[cfg(windows)]
        let out = run_or_park_shell_with_limits(
            Some("powershell"),
            "Start-Sleep -Seconds 8",
            slice,
            hard,
        )
        .unwrap();
        #[cfg(not(windows))]
        let out = run_or_park_shell_with_limits(Some("sh"), "sleep 8", slice, hard).unwrap();
        let id = match out {
            ShellObservation::Running(progress) => progress.id,
            ShellObservation::Finished(result) => {
                assert_eq!(result.code, -1);
                assert!(
                    result.stderr.contains("hard") || result.stderr.contains("timed out"),
                    "{}",
                    result.stderr
                );
                return;
            }
        };
        let started = Instant::now();
        let result = loop {
            match poll_shell_job(&id).unwrap() {
                ShellObservation::Finished(result) => break result,
                ShellObservation::Running(_) if started.elapsed() < Duration::from_secs(5) => {
                    thread::sleep(Duration::from_millis(30));
                }
                ShellObservation::Running(_) => panic!("hard timeout did not settle"),
            }
        };
        assert_eq!(result.code, -1);
        assert!(
            result.stderr.contains("hard") || result.stderr.contains("timed out"),
            "{}",
            result.stderr
        );
    }

    #[test]
    fn parked_job_can_be_cancelled_and_settles() {
        #[cfg(windows)]
        let command = (Some("powershell"), "Start-Sleep -Seconds 8");
        #[cfg(not(windows))]
        let command = (Some("sh"), "sleep 8");
        let observation = run_or_park_shell_with_limits(
            command.0,
            command.1,
            Duration::from_millis(30),
            Duration::from_secs(20),
        )
        .unwrap();
        let ShellObservation::Running(progress) = observation else {
            panic!("long fixture should park");
        };
        let cancelled = cancel_shell_job(&progress.id).unwrap();
        let ShellObservation::Finished(result) = cancelled else {
            panic!("cancellation should settle within its bounded slice");
        };
        assert_eq!(result.code, -1);
        assert!(result.stderr.contains("cancelled"), "{}", result.stderr);
        assert!(!live_shell_job_ids().contains(&progress.id));
    }
}

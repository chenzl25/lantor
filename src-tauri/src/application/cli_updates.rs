//! Version checks and one-tap updates for the Claude Code and Codex CLIs.
//!
//! One install of each CLI serves every agent on that runtime, so the state
//! here is per runtime, not per agent. The update runs the CLI's own
//! `update` command, which knows how the CLI was installed. Running agents
//! keep their current process; each one starts the new version when its warm
//! process next starts (see docs/agent-runtime.md#cli-updates).

use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{process::Command, sync::Mutex};

use crate::app::CommandResult;

const VERSION_TIMEOUT: Duration = Duration::from_secs(20);
const LATEST_TIMEOUT_SECS: u64 = 15;
const UPDATE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const LATEST_TTL: Duration = Duration::from_secs(60 * 60);
const LATEST_FAILURE_TTL: Duration = Duration::from_secs(10 * 60);
const OUTPUT_TAIL_CHARS: usize = 3000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CliRuntimeRequest {
    runtime: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct CliVersionStatus {
    runtime: String,
    command: String,
    installed: Option<String>,
    latest: Option<String>,
    update_available: bool,
    updating: bool,
    /// Why the installed or the latest version is unknown.
    problem: Option<String>,
    /// The last update run by this Lantor process.
    last_update: Option<CliUpdateOutcome>,
    checked_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct CliUpdateOutcome {
    ok: bool,
    /// The command that ran, such as `claude update`.
    command: String,
    from: Option<String>,
    to: Option<String>,
    /// The end of the command's output, plus a note when the version did
    /// not reach the latest release.
    output: String,
    finished_at: String,
}

/// How to reach one CLI. Tests point `program` at a script and `latest_url`
/// at a `file://` URL.
#[derive(Clone, Debug)]
struct CliSpec {
    runtime: String,
    program: String,
    /// Shell and flag that run `exec "$@"`; a login shell in the app so a
    /// Finder-launched process still finds Homebrew and ~/.local/bin.
    shell: (String, String),
    /// npm registry manifest for the release channel the CLI follows.
    latest_url: String,
}

impl CliSpec {
    fn for_runtime(runtime: &str) -> CommandResult<Self> {
        let (program, latest_url) = match runtime {
            "claude" => (
                "claude",
                format!(
                    "https://registry.npmjs.org/@anthropic-ai/claude-code/{}",
                    claude_release_channel()
                ),
            ),
            "codex" => (
                "codex",
                "https://registry.npmjs.org/@openai/codex/latest".to_owned(),
            ),
            _ => return Err(format!("unknown runtime: {runtime}")),
        };
        Ok(Self {
            runtime: runtime.to_owned(),
            program: program.to_owned(),
            shell: ("/bin/zsh".to_owned(), "-lc".to_owned()),
            latest_url,
        })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.shell.0);
        command
            .arg(&self.shell.1)
            .arg("exec \"$@\" 2>&1")
            .arg("lantor-cli")
            .arg(&self.program)
            .args(args)
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
}

/// Claude Code follows `latest` unless settings pick the `stable` channel.
fn claude_release_channel() -> &'static str {
    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")));
    let channel = config_dir
        .and_then(|dir| std::fs::read_to_string(dir.join("settings.json")).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|settings| {
            settings
                .get("autoUpdatesChannel")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    if channel.as_deref() == Some("stable") {
        "stable"
    } else {
        "latest"
    }
}

#[derive(Default)]
struct RuntimeState {
    latest: Option<(Result<String, String>, Instant)>,
    updating: bool,
    last_update: Option<CliUpdateOutcome>,
}

#[derive(Default)]
struct CliUpdates {
    states: Mutex<HashMap<String, RuntimeState>>,
}

fn shared() -> &'static Arc<CliUpdates> {
    static UPDATES: OnceLock<Arc<CliUpdates>> = OnceLock::new();
    UPDATES.get_or_init(Arc::default)
}

pub(crate) async fn load_status(request: CliRuntimeRequest) -> CommandResult<CliVersionStatus> {
    let spec = CliSpec::for_runtime(request.runtime.trim())?;
    Ok(shared().status(&spec).await)
}

pub(crate) async fn start_update(request: CliRuntimeRequest) -> CommandResult<CliVersionStatus> {
    let spec = CliSpec::for_runtime(request.runtime.trim())?;
    Ok(shared().clone().start_update(spec).await)
}

impl CliUpdates {
    async fn status(&self, spec: &CliSpec) -> CliVersionStatus {
        let installed = installed_version(spec).await;
        let latest = self.latest_version(spec).await;
        let states = self.states.lock().await;
        let state = states.get(&spec.runtime);
        build_status(
            spec,
            installed,
            latest,
            state.is_some_and(|state| state.updating),
            state.and_then(|state| state.last_update.clone()),
        )
    }

    /// Starts the CLI's own update command unless one is already running,
    /// and returns the status right away; callers poll `status` for the end.
    async fn start_update(self: Arc<Self>, spec: CliSpec) -> CliVersionStatus {
        let started = {
            let mut states = self.states.lock().await;
            let state = states.entry(spec.runtime.clone()).or_default();
            !std::mem::replace(&mut state.updating, true)
        };
        if started {
            let updates = self.clone();
            let job_spec = spec.clone();
            tokio::spawn(async move { updates.run_update(&job_spec).await });
        }
        self.status(&spec).await
    }

    /// Runs one update to completion. The caller has marked it as updating.
    async fn run_update(&self, spec: &CliSpec) {
        let from = installed_version(spec).await.ok();
        let run = run_with_timeout(spec.command(&["update"]), UPDATE_TIMEOUT).await;
        let to = installed_version(spec).await.ok();
        let latest = self.latest_version(spec).await.ok();
        let outcome = update_outcome(spec, run, from, to, latest);
        let mut states = self.states.lock().await;
        let state = states.entry(spec.runtime.clone()).or_default();
        state.updating = false;
        state.last_update = Some(outcome);
    }

    async fn latest_version(&self, spec: &CliSpec) -> Result<String, String> {
        {
            let states = self.states.lock().await;
            if let Some((cached, at)) = states.get(&spec.runtime).and_then(|s| s.latest.as_ref()) {
                let ttl = if cached.is_ok() {
                    LATEST_TTL
                } else {
                    LATEST_FAILURE_TTL
                };
                if at.elapsed() < ttl {
                    return cached.clone();
                }
            }
        }
        let fetched = fetch_latest_version(&spec.latest_url).await;
        let mut states = self.states.lock().await;
        states.entry(spec.runtime.clone()).or_default().latest =
            Some((fetched.clone(), Instant::now()));
        fetched
    }
}

fn build_status(
    spec: &CliSpec,
    installed: Result<String, String>,
    latest: Result<String, String>,
    updating: bool,
    last_update: Option<CliUpdateOutcome>,
) -> CliVersionStatus {
    let update_available = match (&installed, &latest) {
        (Ok(installed), Ok(latest)) => is_newer(latest, installed),
        _ => false,
    };
    let problem = installed.as_ref().err().or(latest.as_ref().err()).cloned();
    CliVersionStatus {
        runtime: spec.runtime.clone(),
        command: spec.program.clone(),
        installed: installed.ok(),
        latest: latest.ok(),
        update_available,
        updating,
        problem,
        last_update,
        checked_at: now(),
    }
}

struct CommandRun {
    code: Option<i32>,
    output: String,
}

impl CommandRun {
    fn success(&self) -> bool {
        self.code == Some(0)
    }
}

async fn run_with_timeout(mut command: Command, limit: Duration) -> Result<CommandRun, String> {
    let output = tokio::time::timeout(limit, command.output())
        .await
        .map_err(|_| format!("timed out after {} seconds", limit.as_secs()))?
        .map_err(|err| err.to_string())?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(CommandRun {
        code: output.status.code(),
        output: clean_output(&text),
    })
}

async fn installed_version(spec: &CliSpec) -> Result<String, String> {
    let run = run_with_timeout(spec.command(&["--version"]), VERSION_TIMEOUT)
        .await
        .map_err(|err| format!("{} --version {err}", spec.program))?;
    if !run.success() {
        // The shell exits 127 (or 126) when it cannot find or run the CLI.
        return Err(
            if matches!(run.code, Some(126 | 127)) || run.output.is_empty() {
                format!("{} not found on this Mac", spec.program)
            } else {
                first_line(&run.output)
            },
        );
    }
    parse_version(&run.output).ok_or_else(|| {
        format!(
            "unexpected {} --version output: {}",
            spec.program,
            first_line(&run.output)
        )
    })
}

async fn fetch_latest_version(url: &str) -> Result<String, String> {
    let mut command = Command::new("curl");
    command
        .args(["--silent", "--show-error", "--fail", "--location"])
        .args(["--max-time", &LATEST_TIMEOUT_SECS.to_string()])
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(
        Duration::from_secs(LATEST_TIMEOUT_SECS + 5),
        command.output(),
    )
    .await
    .map_err(|_| "Couldn't reach the npm registry".to_owned())?
    .map_err(|err| format!("Couldn't run curl: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "Couldn't reach the npm registry: {}",
            first_line(&String::from_utf8_lossy(&output.stderr))
        ));
    }
    serde_json::from_slice::<Value>(&output.stdout)
        .ok()
        .and_then(|manifest| manifest.get("version")?.as_str().map(str::to_owned))
        .filter(|version| parse_version(version).as_deref() == Some(version.as_str()))
        .ok_or_else(|| "The npm registry returned no version".to_owned())
}

fn update_outcome(
    spec: &CliSpec,
    run: Result<CommandRun, String>,
    from: Option<String>,
    to: Option<String>,
    latest: Option<String>,
) -> CliUpdateOutcome {
    let (mut ok, mut output) = match run {
        Ok(run) => (run.success(), run.output),
        Err(err) => (false, err),
    };
    let note = match (&to, &latest) {
        (None, _) if ok => Some(format!(
            "The update finished, but `{} --version` no longer works.",
            spec.program
        )),
        (Some(to), Some(latest)) if ok && is_newer(latest, to) => Some(format!(
            "The update finished, but {} is still {to}; the latest release is {latest}.",
            spec.program
        )),
        _ => None,
    };
    if let Some(note) = note {
        ok = false;
        output = if output.is_empty() {
            note
        } else {
            format!("{output}\n{note}")
        };
    }
    CliUpdateOutcome {
        ok,
        command: format!("{} update", spec.program),
        from,
        to,
        output: tail_chars(&output, OUTPUT_TAIL_CHARS),
        finished_at: now(),
    }
}

/// The first dotted version in CLI output, such as `2.1.319` in
/// `2.1.319 (Claude Code)` or `0.167.0` in `codex-cli 0.167.0`, including a
/// pre-release suffix like `-alpha.3`.
fn parse_version(text: &str) -> Option<String> {
    text.split(|ch: char| ch.is_whitespace() || ch == '(' || ch == ')' || ch == ',')
        .map(|token| token.trim_start_matches('v'))
        .find(|token| {
            let core = token.split('-').next().unwrap_or("");
            core.split('.').count() >= 2
                && core
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(str::to_owned)
}

/// Whether `candidate` is a later release than `current`. A pre-release
/// sorts before its release; two pre-releases of one version count as equal.
fn is_newer(candidate: &str, current: &str) -> bool {
    fn key(version: &str) -> (Vec<u64>, bool) {
        let (core, pre) = match version.split_once('-') {
            Some((core, _)) => (core, true),
            None => (version, false),
        };
        let numbers = core
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect();
        (numbers, !pre)
    }
    let (candidate_core, candidate_release) = key(candidate);
    let (current_core, current_release) = key(current);
    match candidate_core.cmp(&current_core) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => candidate_release && !current_release,
    }
}

/// Drops ANSI escapes and spinner frames: of each line, keep the text after
/// its last carriage return.
fn clean_output(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        plain.push(ch);
    }
    plain
        .lines()
        .map(|line| line.rsplit('\r').next().unwrap_or("").trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

fn tail_chars(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_owned();
    }
    let tail: String = text.chars().skip(count - limit).collect();
    format!("…{tail}")
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
#[path = "../tests/cli_updates.rs"]
mod tests;

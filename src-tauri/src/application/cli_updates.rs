//! Version checks for the Claude Code and Codex CLIs.
//!
//! One install of each CLI serves every agent on that runtime, so the state
//! here is per runtime, not per agent. Lantor does not install updates
//! itself: the page asks an agent to update its CLI from its shell, where it
//! can deal with whatever goes wrong (see docs/agent-runtime.md#cli-updates).

use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::OnceLock,
    time::{Duration, Instant},
};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{process::Command, sync::Mutex};

use crate::app::CommandResult;

const VERSION_TIMEOUT: Duration = Duration::from_secs(20);
const LATEST_TIMEOUT_SECS: u64 = 15;
const LATEST_TTL: Duration = Duration::from_secs(60 * 60);
const LATEST_FAILURE_TTL: Duration = Duration::from_secs(10 * 60);

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
    /// Why the installed or the latest version is unknown.
    problem: Option<String>,
    checked_at: String,
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

/// A latest release, or why it could not be fetched, and when.
type LatestFetch = (Result<String, String>, Instant);

/// The latest release per runtime.
#[derive(Default)]
struct CliVersions {
    latest: Mutex<HashMap<String, LatestFetch>>,
}

fn shared() -> &'static CliVersions {
    static UPDATES: OnceLock<CliVersions> = OnceLock::new();
    UPDATES.get_or_init(CliVersions::default)
}

pub(crate) async fn load_status(request: CliRuntimeRequest) -> CommandResult<CliVersionStatus> {
    let spec = CliSpec::for_runtime(request.runtime.trim())?;
    Ok(shared().status(&spec).await)
}

impl CliVersions {
    async fn status(&self, spec: &CliSpec) -> CliVersionStatus {
        let installed = installed_version(spec).await;
        let latest = self.latest_version(spec).await;
        build_status(spec, installed, latest)
    }

    async fn latest_version(&self, spec: &CliSpec) -> Result<String, String> {
        {
            let latest = self.latest.lock().await;
            if let Some((cached, at)) = latest.get(&spec.runtime) {
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
        self.latest
            .lock()
            .await
            .insert(spec.runtime.clone(), (fetched.clone(), Instant::now()));
        fetched
    }
}

fn build_status(
    spec: &CliSpec,
    installed: Result<String, String>,
    latest: Result<String, String>,
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
        problem,
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

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
#[path = "../tests/cli_updates.rs"]
mod tests;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
};

use crate::{
    app::{to_string, CommandResult},
    db::expand_home_path,
    models::RuntimeCheck,
};
use tauri::Manager;

#[derive(Debug, PartialEq, Eq)]
enum OpenLinkTarget {
    External(String),
    LocalFile { path: String, line: Option<u32> },
}

impl OpenLinkTarget {
    fn system_open_target(&self) -> &str {
        match self {
            OpenLinkTarget::External(url) => url,
            OpenLinkTarget::LocalFile { path, .. } => path,
        }
    }
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn percent_decode_utf8(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push(hex_value(high)? << 4 | hex_value(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

#[cfg(target_os = "macos")]
fn percent_encode_uri_path(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

fn normalize_external_url(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once(':')?;
    let scheme = scheme.to_ascii_lowercase();
    match scheme.as_str() {
        "http" | "https" if rest.starts_with("//") => Some(url.to_owned()),
        "mailto" if !rest.is_empty() => Some(url.to_owned()),
        "file" if rest.starts_with("//") => Some(url.to_owned()),
        _ => None,
    }
}

fn parse_line_number(value: &str) -> Option<u32> {
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    value.parse::<u32>().ok().filter(|line| *line > 0)
}

fn split_local_path_line_suffix(value: &str) -> (&str, Option<u32>) {
    if Path::new(value).exists() {
        return (value, None);
    }

    if let Some((path, line)) = value.rsplit_once(':') {
        if let Some(line) = parse_line_number(line) {
            if Path::new(path).exists() {
                return (path, Some(line));
            }
        }
    }

    if let Some((path, line)) = value.rsplit_once("#L") {
        if let Some(line) = parse_line_number(line) {
            if Path::new(path).exists() {
                return (path, Some(line));
            }
        }
    }

    (value, None)
}

fn normalize_local_path_candidate(url: &str) -> Option<OpenLinkTarget> {
    if url.chars().any(char::is_control) {
        return None;
    }
    let expanded = expand_home_path(url);
    let (path_candidate, line) = split_local_path_line_suffix(&expanded);
    let path = Path::new(path_candidate);
    if path.is_absolute() && path.exists() {
        return Some(OpenLinkTarget::LocalFile {
            path: path.to_string_lossy().to_string(),
            line,
        });
    }
    None
}

fn normalize_local_path_link(url: &str) -> Option<OpenLinkTarget> {
    normalize_local_path_candidate(url).or_else(|| {
        percent_decode_utf8(url).and_then(|decoded| normalize_local_path_candidate(&decoded))
    })
}

fn normalize_open_link_target(url: &str) -> Option<OpenLinkTarget> {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.len() > 4096 || trimmed.chars().any(char::is_control) {
        return None;
    }

    normalize_external_url(trimmed)
        .map(OpenLinkTarget::External)
        .or_else(|| normalize_local_path_link(trimmed))
}

fn open_link_target_with_system(target: &str) -> CommandResult<()> {
    #[cfg(target_os = "macos")]
    let status = StdCommand::new("open")
        .arg(target)
        .status()
        .map_err(to_string)?;

    #[cfg(target_os = "windows")]
    let status = StdCommand::new("cmd")
        .args(["/C", "start", "", target])
        .status()
        .map_err(to_string)?;

    #[cfg(all(unix, not(target_os = "macos")))]
    let status = StdCommand::new("xdg-open")
        .arg(target)
        .status()
        .map_err(to_string)?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("failed to open link target: {status}"))
    }
}

fn editor_command_candidates() -> [&'static str; 3] {
    ["cursor", "code", "code-insiders"]
}

#[cfg(target_os = "macos")]
fn resolve_editor_command(command: &str) -> String {
    // GUI apps launched by Finder do not always inherit the shell PATH.
    let script = format!("command -v {command}");
    let output = StdCommand::new("/bin/zsh")
        .arg("-lc")
        .arg(script)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !path.is_empty() {
                return path;
            }
        }
    }

    command.to_owned()
}

#[cfg(not(target_os = "macos"))]
fn resolve_editor_command(command: &str) -> String {
    command.to_owned()
}

fn open_local_file_at_line_with_editor(path: &str, line: u32) -> Option<CommandResult<()>> {
    let target = format!("{path}:{line}");
    let mut attempted = false;

    for command in editor_command_candidates() {
        let command = resolve_editor_command(command);
        let status = StdCommand::new(command).arg("-g").arg(&target).status();
        match status {
            Ok(status) if status.success() => return Some(Ok(())),
            Ok(_) => attempted = true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => attempted = true,
        }
    }

    if attempted {
        Some(Err(
            "failed to open local file at line with editor".to_owned()
        ))
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn editor_file_uri(scheme: &str, path: &str, line: u32) -> String {
    let path = percent_encode_uri_path(path);
    format!("{scheme}://file{path}:{line}:1")
}

#[cfg(target_os = "macos")]
fn open_local_file_at_line_with_editor_uri(path: &str, line: u32) -> Option<CommandResult<()>> {
    let mut attempted = false;

    for scheme in ["cursor", "vscode", "vscode-insiders"] {
        let uri = editor_file_uri(scheme, path, line);
        match open_link_target_with_system(&uri) {
            Ok(()) => return Some(Ok(())),
            Err(_) => attempted = true,
        }
    }

    if attempted {
        Some(Err(
            "failed to open local file at line with editor URL".to_owned()
        ))
    } else {
        None
    }
}

#[cfg(not(target_os = "macos"))]
fn open_local_file_at_line_with_editor_uri(_path: &str, _line: u32) -> Option<CommandResult<()>> {
    None
}

fn open_link_target(target: &OpenLinkTarget) -> CommandResult<()> {
    if let OpenLinkTarget::LocalFile {
        path,
        line: Some(line),
    } = target
    {
        if let Some(result) = open_local_file_at_line_with_editor(path, *line) {
            if result.is_ok() {
                return result;
            }
        }
        if let Some(result) = open_local_file_at_line_with_editor_uri(path, *line) {
            if result.is_ok() {
                return result;
            }
        }
    }

    open_link_target_with_system(target.system_open_target())
}

#[tauri::command]
pub(crate) async fn open_external_url(url: String) -> CommandResult<()> {
    let target = normalize_open_link_target(&url).ok_or_else(|| {
        "only http, https, mailto, file, and existing local file links can be opened".to_owned()
    })?;
    tauri::async_runtime::spawn_blocking(move || open_link_target(&target))
        .await
        .map_err(to_string)?
}

fn safe_download_filename(value: &str) -> String {
    let name = Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment")
        .trim();
    let sanitized: String = name
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            character if character.is_control() => '_',
            character => character,
        })
        .collect();
    let sanitized = sanitized.trim_matches(['.', ' ']);
    if sanitized.is_empty() {
        "attachment".to_owned()
    } else {
        sanitized.to_owned()
    }
}

fn unique_download_path(downloads_dir: &Path, filename: &str) -> PathBuf {
    let initial = downloads_dir.join(filename);
    if !initial.exists() {
        return initial;
    }

    let source = Path::new(filename);
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment");
    let extension = source.extension().and_then(|value| value.to_str());
    for index in 1..1000 {
        let candidate_name = match extension {
            Some(extension) if !extension.is_empty() => format!("{stem} ({index}).{extension}"),
            _ => format!("{stem} ({index})"),
        };
        let candidate = downloads_dir.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    downloads_dir.join(format!("{stem} ({})", uuid::Uuid::new_v4()))
}

#[tauri::command]
pub(crate) async fn download_attachment(
    app: tauri::AppHandle,
    storage_path: String,
    original_name: String,
) -> CommandResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let source = PathBuf::from(expand_home_path(&storage_path));
        if !source.is_file() {
            return Err(format!(
                "attachment file does not exist: {}",
                source.display()
            ));
        }
        let downloads_dir = app.path().download_dir().map_err(to_string)?;
        fs::create_dir_all(&downloads_dir).map_err(to_string)?;
        let filename = safe_download_filename(&original_name);
        let target = unique_download_path(&downloads_dir, &filename);
        fs::copy(&source, &target).map_err(to_string)?;
        Ok(target.to_string_lossy().to_string())
    })
    .await
    .map_err(to_string)?
}

#[tauri::command]
pub(crate) async fn check_runtime(runtime: String) -> CommandResult<RuntimeCheck> {
    check_runtime_in_env(runtime).await
}

pub(crate) async fn check_runtime_in_env(runtime: String) -> CommandResult<RuntimeCheck> {
    let runtime = runtime.trim().to_owned();
    let command = match runtime.as_str() {
        "codex" => "codex",
        "claude" => "claude",
        _ => {
            return Ok(RuntimeCheck {
                runtime,
                command: String::new(),
                available: false,
                detail: "Unknown runtime".to_owned(),
            });
        }
    };

    let script = format!(
        "if command -v {command} >/dev/null 2>&1; then {command} --version 2>&1 | head -n 1; else echo '{command} not found in PATH' >&2; exit 127; fi"
    );
    let output = StdCommand::new("/bin/zsh")
        .arg("-lc")
        .arg(script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(to_string)?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let detail = if !stdout.is_empty() {
        stdout
    } else if !stderr.is_empty() {
        stderr
    } else if output.status.success() {
        format!("{command} found")
    } else {
        format!("{command} unavailable")
    };

    Ok(RuntimeCheck {
        runtime,
        command: command.to_owned(),
        available: output.status.success(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{normalize_open_link_target, OpenLinkTarget};

    #[test]
    fn open_link_target_normalization_allows_safe_schemes() {
        assert_eq!(
            normalize_open_link_target(" https://example.com/path?q=1 "),
            Some(OpenLinkTarget::External(
                "https://example.com/path?q=1".to_owned()
            ))
        );
        assert_eq!(
            normalize_open_link_target("mailto:hello@example.com"),
            Some(OpenLinkTarget::External(
                "mailto:hello@example.com".to_owned()
            ))
        );
        assert_eq!(
            normalize_open_link_target("file:///tmp/report.txt"),
            Some(OpenLinkTarget::External(
                "file:///tmp/report.txt".to_owned()
            ))
        );
        assert!(normalize_open_link_target("javascript:alert(1)").is_none());
        assert!(normalize_open_link_target("https://example.com/\nopen").is_none());
    }

    #[test]
    fn open_link_target_normalization_allows_existing_local_paths_with_line_suffixes() {
        let dir = std::env::temp_dir().join(format!("lantor-link-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let file = file.to_string_lossy().to_string();

        assert_eq!(
            normalize_open_link_target(&file),
            Some(OpenLinkTarget::LocalFile {
                path: file.clone(),
                line: None
            })
        );
        assert_eq!(
            normalize_open_link_target(&format!("{file}:42")),
            Some(OpenLinkTarget::LocalFile {
                path: file.clone(),
                line: Some(42)
            })
        );
        assert_eq!(
            normalize_open_link_target(&format!("{file}#L42")),
            Some(OpenLinkTarget::LocalFile {
                path: file.clone(),
                line: Some(42)
            })
        );
        assert!(normalize_open_link_target("/definitely/not/a/lantor/file.rs:1").is_none());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_link_target_normalization_allows_percent_encoded_local_paths_with_spaces() {
        let dir = std::env::temp_dir().join(format!("lantor-link test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let file = file.to_string_lossy().to_string();
        let encoded = file.replace(' ', "%20");

        assert_eq!(
            normalize_open_link_target(&encoded),
            Some(OpenLinkTarget::LocalFile {
                path: file.clone(),
                line: None
            })
        );
        assert_eq!(
            normalize_open_link_target(&format!("{encoded}:42")),
            Some(OpenLinkTarget::LocalFile {
                path: file,
                line: Some(42)
            })
        );
        assert!(normalize_open_link_target("/tmp/lantor%0Ainvalid").is_none());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn editor_file_uri_percent_encodes_local_paths() {
        assert_eq!(
            super::editor_file_uri("vscode", "/tmp/dir with space/main.rs", 42),
            "vscode://file/tmp/dir%20with%20space/main.rs:42:1"
        );
        assert_eq!(
            super::editor_file_uri("cursor", "/tmp/100%/main.rs", 7),
            "cursor://file/tmp/100%25/main.rs:7:1"
        );
    }
}

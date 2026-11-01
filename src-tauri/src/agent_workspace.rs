use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{
    app::{to_string, CommandResult},
    models::{AgentWorkspaceEntry, AgentWorkspaceFile, AgentWorkspaceListing},
};

const AGENT_WORKSPACE_PREVIEW_LIMIT: usize = 256 * 1024;

pub(crate) struct AgentWorkspaceSummary {
    pub(crate) exists: bool,
    pub(crate) memory_path: String,
    pub(crate) memory_exists: bool,
    pub(crate) entries: Vec<AgentWorkspaceEntry>,
}

pub(crate) fn load_agent_workspace_summary(working_directory: &str) -> AgentWorkspaceSummary {
    let working_directory = working_directory.trim();
    if working_directory.is_empty() {
        return AgentWorkspaceSummary {
            exists: false,
            memory_path: String::new(),
            memory_exists: false,
            entries: Vec::new(),
        };
    }

    let workspace = PathBuf::from(working_directory);
    let memory_path = workspace.join("MEMORY.md");
    let memory_path_string = memory_path.to_string_lossy().to_string();
    let exists = workspace.is_dir();
    let memory_exists = memory_path.is_file();
    let mut entries = Vec::new();

    if exists {
        if let Ok(read_dir) = fs::read_dir(&workspace) {
            for entry in read_dir.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_string();
                if should_hide_workspace_entry(&file_name) {
                    continue;
                }
                if let Ok(metadata) = entry.metadata() {
                    let kind = if metadata.is_dir() {
                        "dir"
                    } else if metadata.is_file() {
                        "file"
                    } else {
                        "other"
                    };
                    let path = entry.path();
                    entries.push(workspace_entry_from_path(
                        &workspace, &path, file_name, kind, &metadata,
                    ));
                }
            }
        }
    }

    sort_workspace_entries(&mut entries);
    entries.truncate(48);

    AgentWorkspaceSummary {
        exists,
        memory_path: memory_path_string,
        memory_exists,
        entries,
    }
}

fn should_hide_workspace_entry(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | "dist" | ".next" | ".turbo"
    )
}

fn workspace_entry_from_path(
    workspace: &Path,
    path: &Path,
    name: String,
    kind: &str,
    metadata: &fs::Metadata,
) -> AgentWorkspaceEntry {
    AgentWorkspaceEntry {
        name,
        path: path.to_string_lossy().to_string(),
        relative_path: path
            .strip_prefix(workspace)
            .ok()
            .map(path_to_slash_string)
            .unwrap_or_default(),
        kind: kind.to_owned(),
        size_bytes: metadata.is_file().then_some(metadata.len() as i64),
    }
}

fn path_to_slash_string(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

async fn agent_workspace_root(pool: &SqlitePool, agent_id: Uuid) -> CommandResult<PathBuf> {
    let row = sqlx::query("select working_directory from agents where id = $1")
        .bind(agent_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
        .ok_or_else(|| "agent not found".to_owned())?;
    let working_directory: String = row.get("working_directory");
    let working_directory = working_directory.trim();
    if working_directory.is_empty() {
        return Err("agent working directory is not configured".to_owned());
    }
    let workspace = PathBuf::from(working_directory);
    let canonical = workspace
        .canonicalize()
        .map_err(|err| format!("workspace is not available: {err}"))?;
    if !canonical.is_dir() {
        return Err("agent workspace is not a directory".to_owned());
    }
    Ok(canonical)
}

fn safe_workspace_path(workspace: &Path, relative_path: &str) -> CommandResult<PathBuf> {
    let relative_path = relative_path.trim();
    let relative = Path::new(relative_path);
    if relative.is_absolute() {
        return Err("workspace path must be relative".to_owned());
    }

    let mut clean = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => clean.push(value),
            Component::CurDir => {}
            _ => return Err("workspace path cannot escape the workspace".to_owned()),
        }
    }

    let target = workspace.join(clean);
    let canonical = target
        .canonicalize()
        .map_err(|err| format!("workspace path is not available: {err}"))?;
    if !canonical.starts_with(workspace) {
        return Err("workspace path cannot escape the workspace".to_owned());
    }
    Ok(canonical)
}

fn list_workspace_entries(
    workspace: &Path,
    directory: &Path,
) -> CommandResult<Vec<AgentWorkspaceEntry>> {
    let mut entries = Vec::new();
    let read_dir = fs::read_dir(directory).map_err(to_string)?;
    for entry in read_dir.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if should_hide_workspace_entry(&file_name) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let kind = if metadata.is_dir() {
            "dir"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };
        entries.push(workspace_entry_from_path(
            workspace,
            &entry.path(),
            file_name,
            kind,
            &metadata,
        ));
    }
    sort_workspace_entries(&mut entries);
    entries.truncate(200);
    Ok(entries)
}

fn sort_workspace_entries(entries: &mut [AgentWorkspaceEntry]) {
    entries.sort_by(
        |left, right| match (left.kind.as_str(), right.kind.as_str()) {
            ("dir", "file") | ("dir", "other") | ("file", "other") => std::cmp::Ordering::Less,
            ("file", "dir") | ("other", "dir") | ("other", "file") => std::cmp::Ordering::Greater,
            _ => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
        },
    );
}

pub(crate) async fn agent_workspace_list_in_pool(
    pool: &SqlitePool,
    agent_id: Uuid,
    path: &str,
) -> CommandResult<AgentWorkspaceListing> {
    let workspace = agent_workspace_root(pool, agent_id).await?;
    let directory = safe_workspace_path(&workspace, path)?;
    if !directory.is_dir() {
        return Err("workspace path is not a directory".to_owned());
    }
    Ok(AgentWorkspaceListing {
        path: path_to_slash_string(
            directory
                .strip_prefix(&workspace)
                .unwrap_or_else(|_| Path::new("")),
        ),
        entries: list_workspace_entries(&workspace, &directory)?,
    })
}

pub(crate) async fn agent_workspace_read_file_in_pool(
    pool: &SqlitePool,
    agent_id: Uuid,
    path: &str,
) -> CommandResult<AgentWorkspaceFile> {
    let workspace = agent_workspace_root(pool, agent_id).await?;
    let file_path = safe_workspace_path(&workspace, path)?;
    if !file_path.is_file() {
        return Err("workspace path is not a file".to_owned());
    }

    let metadata = fs::metadata(&file_path).map_err(to_string)?;
    let (content, truncated) = read_workspace_preview(&file_path)?;

    let name = file_path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_owned());
    let relative_path = path_to_slash_string(
        file_path
            .strip_prefix(&workspace)
            .unwrap_or_else(|_| Path::new("")),
    );
    Ok(AgentWorkspaceFile {
        name,
        path: file_path.to_string_lossy().to_string(),
        relative_path,
        size_bytes: metadata.len() as i64,
        language: workspace_preview_language(&file_path),
        content,
        truncated,
    })
}

fn read_workspace_preview(path: &Path) -> CommandResult<(String, bool)> {
    let file = fs::File::open(path).map_err(to_string)?;
    let mut bytes = Vec::with_capacity(AGENT_WORKSPACE_PREVIEW_LIMIT + 1);
    file.take((AGENT_WORKSPACE_PREVIEW_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(to_string)?;
    let truncated = bytes.len() > AGENT_WORKSPACE_PREVIEW_LIMIT;
    if truncated {
        bytes.truncate(AGENT_WORKSPACE_PREVIEW_LIMIT);
    }

    let mut content = match String::from_utf8(bytes) {
        Ok(content) => content,
        Err(err) if truncated && err.utf8_error().error_len().is_none() => {
            let valid_up_to = err.utf8_error().valid_up_to();
            let mut bytes = err.into_bytes();
            bytes.truncate(valid_up_to);
            String::from_utf8(bytes).expect("validated UTF-8 prefix")
        }
        Err(err) => {
            return Err(format!(
                "workspace preview only supports UTF-8 text files: {}",
                err.utf8_error()
            ));
        }
    };
    if truncated {
        content.push_str("\n\n[preview truncated by Lantor]");
    }
    Ok((content, truncated))
}

fn workspace_preview_language(path: &Path) -> String {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => "markdown",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "css" => "css",
        "html" => "html",
        "sql" => "sql",
        "py" => "python",
        "sh" | "zsh" | "bash" => "shell",
        _ => "text",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use uuid::Uuid;

    use super::{read_workspace_preview, AGENT_WORKSPACE_PREVIEW_LIMIT};

    #[test]
    fn workspace_preview_reads_only_a_bounded_utf8_prefix() {
        let path =
            std::env::temp_dir().join(format!("lantor-workspace-preview-{}.txt", Uuid::new_v4()));
        let mut body = "a".repeat(AGENT_WORKSPACE_PREVIEW_LIMIT - 1);
        body.push('🙂');
        body.push_str(&"z".repeat(1024));
        fs::write(&path, body).unwrap();

        let (content, truncated) = read_workspace_preview(&path).unwrap();
        let _ = fs::remove_file(path);

        assert!(truncated);
        assert!(content.starts_with(&"a".repeat(AGENT_WORKSPACE_PREVIEW_LIMIT - 1)));
        assert!(content.ends_with("[preview truncated by Lantor]"));
        assert!(!content.contains('🙂'));
    }
}

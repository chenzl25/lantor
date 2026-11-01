use std::fs;
use std::path::{Path, PathBuf};

/// Tables whose `status` (and, for runs, `stopped_at`) only this module writes.
const LIFECYCLE_TABLES: [&str; 3] = ["agent_runs", "agent_work_items", "agents"];
const LIFECYCLE_COLUMNS: [&str; 2] = ["status", "stopped_at"];

#[test]
fn only_the_lifecycle_module_writes_run_work_item_and_agent_statuses() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rust_files(&source_root, &mut files);
    assert!(files.len() > 50, "found only {} source files", files.len());

    let mut offenders = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(&source_root)
            .expect("file under src")
            .to_string_lossy()
            .replace('\\', "/");
        if is_exempt(&relative) {
            continue;
        }
        let source = fs::read_to_string(&file).expect("read source file");
        for line in status_writes(without_inline_tests(&source)) {
            offenders.push(format!("src/{relative}:{line}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "run, work item and agent statuses change only in runtime/lifecycle.rs; \
         add a transition there instead of writing them in:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn status_write_scan_finds_assignments_and_ignores_filters() {
    let writes = r##"
        sqlx::query("update agents set status = 'idle' where id = $1")
        sqlx::query(
            r#"
            UPDATE agent_runs
            SET log = $2,
                stopped_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            WHERE id = $1
            "#,
        )
        sqlx::query("update agent_work_items set status=$2 where id = $1")
    "##;
    assert_eq!(status_writes(writes), [2, 5, 11]);

    let reads_and_other_columns = r#"
        sqlx::query("update agent_work_items set run_id = $2 where id = $1 and status = 'running'")
        sqlx::query("update agents set status_detail = $2, avatar = $3 where id = $1")
        sqlx::query("update agent_runs set log = $2 where id = $1 returning id, status")
        sqlx::query("update agent_held_outputs set state = 'requeued' where id = $1")
        sqlx::query("select status from agents where id = $1")
    "#;
    assert_eq!(status_writes(reads_and_other_columns), Vec::<usize>::new());
}

fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read source directory") {
        let path = entry.expect("source directory entry").path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

/// This module, and test code, which sets up statuses as fixtures.
fn is_exempt(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    relative == "runtime/lifecycle.rs"
        || relative == "test_support.rs"
        || relative.starts_with("tests/")
        || name == "tests.rs"
        || name.ends_with("_tests.rs")
}

/// The source up to its first inline `#[cfg(test)] mod name { ... }`. Inline
/// test modules sit at the end of their file.
fn without_inline_tests(source: &str) -> &str {
    let mut from = 0;
    while let Some(found) = source[from..].find("#[cfg(test)]") {
        let at = from + found;
        from = at + "#[cfg(test)]".len();
        let rest = source[from..].trim_start();
        if !rest.starts_with("mod ") {
            continue;
        }
        let declaration = &rest[..rest.find('\n').unwrap_or(rest.len())];
        if declaration.contains('{') {
            return &source[..at];
        }
    }
    source
}

/// Lines of `update <lifecycle table> set ...` statements whose SET clause
/// assigns a lifecycle column.
fn status_writes(source: &str) -> Vec<usize> {
    let lower = source.to_ascii_lowercase();
    let mut lines = Vec::new();
    let mut from = 0;
    while let Some(found) = find_word(&lower[from..], "update") {
        let start = from + found;
        from = start + "update".len();
        let rest = lower[from..].trim_start();
        let Some(table) = LIFECYCLE_TABLES
            .iter()
            .find(|table| starts_with_word(rest, table))
        else {
            continue;
        };
        let statement = &rest[table.len()..];
        let statement = &statement[..statement.find(['"', ';']).unwrap_or(statement.len())];
        let Some(set_at) = find_word(statement, "set") else {
            continue;
        };
        let assignments = &statement[set_at + "set".len()..];
        let assignments =
            &assignments[..find_word(assignments, "where").unwrap_or(assignments.len())];
        if LIFECYCLE_COLUMNS
            .iter()
            .any(|column| assigns(assignments, column))
        {
            lines.push(source[..start].matches('\n').count() + 1);
        }
    }
    lines
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn starts_with_word(text: &str, word: &str) -> bool {
    text.starts_with(word)
        && !text
            .as_bytes()
            .get(word.len())
            .is_some_and(|byte| is_identifier_byte(*byte))
}

fn find_word(text: &str, word: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = text[from..].find(word) {
        let at = from + found;
        let before_ok = at == 0 || !is_identifier_byte(text.as_bytes()[at - 1]);
        if before_ok && starts_with_word(&text[at..], word) {
            return Some(at);
        }
        from = at + word.len();
    }
    None
}

fn assigns(assignments: &str, column: &str) -> bool {
    let mut from = 0;
    while let Some(found) = find_word(&assignments[from..], column) {
        let after = from + found + column.len();
        if assignments[after..].trim_start().starts_with('=') {
            return true;
        }
        from = after;
    }
    false
}

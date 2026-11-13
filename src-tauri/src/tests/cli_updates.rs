use std::path::PathBuf;

use uuid::Uuid;

use super::{clean_output, is_newer, parse_version, CliSpec, CliVersions};

/// A fake CLI that prints the version kept in a file next to it.
const FAKE_CLI: &str = r#"#!/bin/sh
dir=$(dirname "$0")
case "$1" in
  --version) printf '%s (Fake CLI)\n' "$(cat "$dir/version")" ;;
esac
"#;

struct FakeCli {
    dir: PathBuf,
    spec: CliSpec,
}

impl FakeCli {
    fn new(version: &str, latest: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("lantor-cli-updates-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("fake-cli");
        std::fs::write(&program, FAKE_CLI).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let fake = Self {
            spec: CliSpec {
                runtime: "claude".to_owned(),
                program: program.to_string_lossy().into_owned(),
                shell: ("/bin/sh".to_owned(), "-c".to_owned()),
                latest_url: format!("file://{}", dir.join("latest.json").display()),
            },
            dir,
        };
        fake.write("version", version);
        fake.publish(latest);
        fake
    }

    fn write(&self, name: &str, contents: &str) {
        std::fs::write(self.dir.join(name), contents).unwrap();
    }

    fn publish(&self, latest: &str) {
        self.write(
            "latest.json",
            &format!(r#"{{"name":"fake","version":"{latest}"}}"#),
        );
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for FakeCli {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn versions_are_read_from_cli_output() {
    assert_eq!(
        parse_version("2.1.319 (Claude Code)").as_deref(),
        Some("2.1.319")
    );
    assert_eq!(
        parse_version("codex-cli 0.167.0").as_deref(),
        Some("0.167.0")
    );
    assert_eq!(
        parse_version("codex-cli 0.169.0-alpha.3").as_deref(),
        Some("0.169.0-alpha.3")
    );
    assert_eq!(parse_version("tool v1.4\n").as_deref(), Some("1.4"));
    assert_eq!(parse_version("zsh:1: command not found: claude"), None);
}

#[test]
fn only_a_later_release_counts_as_newer() {
    assert!(is_newer("2.1.320", "2.1.319"));
    assert!(is_newer("2.1.10", "2.1.9"));
    assert!(is_newer("0.168.0", "0.167.9"));
    assert!(is_newer("0.169.0", "0.169.0-alpha.3"));
    assert!(!is_newer("2.1.319", "2.1.319"));
    // The stable channel trails latest; a newer install is not an update.
    assert!(!is_newer("2.1.311", "2.1.319"));
    assert!(is_newer("0.169.0-alpha.3", "0.167.0"));
    assert!(!is_newer("0.169.0-alpha.3", "0.169.0"));
    assert!(!is_newer("0.169.0-alpha.4", "0.169.0-alpha.3"));
}

#[test]
fn cli_output_drops_colors_and_spinner_frames() {
    let raw = "Current version: 1.0.0\n\u{1b}[32mChecking\u{1b}[0m |\rChecking /\rChecking done\n\u{1b}[1mUpdated\u{1b}[22m  \n";
    assert_eq!(
        clean_output(raw),
        "Current version: 1.0.0\nChecking done\nUpdated"
    );
}

#[tokio::test]
async fn status_offers_an_update_when_the_registry_has_a_later_release() {
    let fake = FakeCli::new("1.2.3", "1.2.10");
    let versions = CliVersions::default();
    let status = versions.status(&fake.spec).await;
    assert_eq!(status.installed.as_deref(), Some("1.2.3"));
    assert_eq!(status.latest.as_deref(), Some("1.2.10"));
    assert!(status.update_available);
    assert_eq!(status.problem, None);

    // The latest release is cached; the installed version is read each time.
    fake.publish("1.2.11");
    fake.write("version", "1.2.10");
    let status = versions.status(&fake.spec).await;
    assert_eq!(status.latest.as_deref(), Some("1.2.10"));
    assert_eq!(status.installed.as_deref(), Some("1.2.10"));
    assert!(!status.update_available);
}

#[tokio::test]
async fn status_explains_a_missing_cli_or_an_unreachable_registry() {
    let fake = FakeCli::new("1.2.3", "1.2.10");
    let missing = CliSpec {
        program: fake.path("missing-cli").to_string_lossy().into_owned(),
        ..fake.spec.clone()
    };
    let status = CliVersions::default().status(&missing).await;
    assert_eq!(status.installed, None);
    assert!(!status.update_available);
    assert!(
        status
            .problem
            .as_deref()
            .is_some_and(|problem| problem.contains("not found")),
        "{status:?}"
    );

    std::fs::remove_file(fake.path("latest.json")).unwrap();
    let status = CliVersions::default().status(&fake.spec).await;
    assert_eq!(status.installed.as_deref(), Some("1.2.3"));
    assert_eq!(status.latest, None);
    assert!(!status.update_available);
    assert!(
        status
            .problem
            .as_deref()
            .is_some_and(|problem| problem.starts_with("Couldn't reach")),
        "{status:?}"
    );
}

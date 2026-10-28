//! A Project gate runs the exact release its manifest pins before current rules can edit it.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use assert_cmd::Command;
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    repository: PathBuf,
    cache: PathBuf,
    record: PathBuf,
    binary: PathBuf,
}

impl Fixture {
    fn new(pin: &str, child_exit: u8) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).unwrap();
        let repository = fs::canonicalize(repository).unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());
        fs::write(repository.join("README.md"), "# Synthetic Project  \n").unwrap();
        fs::write(
            repository.join("navigator.yaml"),
            format!("version: {pin}\nproject:\n  host: gates.example.test\n  name: synthetic\n"),
        )
        .unwrap();
        let cache = temp.path().join("cache");
        let platform = match std::env::consts::OS {
            "macos" => "macos",
            "linux" => "linux",
            other => panic!("unsupported test host: {other}"),
        };
        let binary = cache
            .join("navigator/releases")
            .join(pin)
            .join(platform)
            .join("navigator");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nif [ -n \"${{NAVIGATOR_RELEASE_TAG:-}}\" ]; then exit 91; fi\nif [ \"$1\" = --version ]; then printf 'navigator {pin}\\n'; exit 0; fi\nprintf '%s\\n' \"$PWD\" \"$@\" > \"$TEST_GATE_RECORD\"\nprintf 'pinned-gate-output\\n'\nprintf 'pinned-gate-diagnostic\\n' >&2\nexit {child_exit}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        let record = temp.path().join("invoked");
        Self {
            _temp: temp,
            repository,
            cache,
            record,
            binary,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("navigator").unwrap();
        command
            .current_dir(&self.repository)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_CACHE_HOME", &self.cache)
            .env("NAVIGATOR_RELEASE_TAG", "26.10.28")
            .env("TEST_GATE_RECORD", &self.record);
        command
    }
}

#[test]
fn a_mismatched_release_delegates_before_current_rules_or_autofixes() {
    let fixture = Fixture::new("26.10.24", 0);
    let output = fixture
        .command()
        .args(["project", "gate"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"pinned-gate-output\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("pinned-gate-diagnostic"));
    assert_eq!(
        fs::read_to_string(&fixture.record).unwrap(),
        format!("{}\nproject\ngate\n", fixture.repository.display())
    );
    assert_eq!(
        fs::read(fixture.repository.join("README.md")).unwrap(),
        b"# Synthetic Project  \n",
        "the current binary must not autofix files before dispatch"
    );
}

#[test]
fn the_pinned_release_receives_flags_and_its_failure_is_preserved() {
    let fixture = Fixture::new("26.10.24", 17);
    let output = fixture
        .command()
        .env("CI", "true")
        .args(["project", "gate", "--ci", "--check", "--deep"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(17), "{output:?}");
    assert!(fs::read_to_string(&fixture.record)
        .unwrap()
        .ends_with("project\ngate\n--ci\n--check\n--deep\n"));
}

#[test]
fn tracked_file_checks_also_use_the_pinned_release() {
    let fixture = Fixture::new("26.10.24", 17);
    let output = fixture
        .command()
        .args(["project", "gate", "--tracked-files"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(17), "{output:?}");
    assert!(fs::read_to_string(&fixture.record)
        .unwrap()
        .ends_with("project\ngate\n--tracked-files\n"));
}

#[test]
fn the_matching_release_does_not_run_an_unnecessary_cached_binary() {
    let fixture = Fixture::new("26.10.28", 17);
    let output = fixture
        .command()
        .args(["project", "gate", "--tracked-files"])
        .output()
        .unwrap();
    assert_ne!(output.status.code(), Some(17), "{output:?}");
    assert!(!fixture.record.exists());
}

#[test]
fn a_wrong_cached_release_blocks_instead_of_running_current_rules() {
    let fixture = Fixture::new("26.10.24", 0);
    fs::write(
        &fixture.binary,
        "#!/bin/sh\nprintf 'navigator 26.10.23\\n'\n",
    )
    .unwrap();
    let output = fixture
        .command()
        .args(["project", "gate"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not report navigator 26.10.24"));
    assert!(!fixture.record.exists());
    assert_eq!(
        fs::read(fixture.repository.join("README.md")).unwrap(),
        b"# Synthetic Project  \n"
    );
}

#[test]
fn an_invalid_pin_blocks_before_current_rules_can_edit_content() {
    let fixture = Fixture::new("invalid-release", 0);
    let output = fixture
        .command()
        .args(["project", "gate"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!fixture.record.exists());
    assert_eq!(
        fs::read(fixture.repository.join("README.md")).unwrap(),
        b"# Synthetic Project  \n"
    );
}

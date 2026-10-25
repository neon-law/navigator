//! Public self-hosting commands must work with no inherited credentials.
use std::path::Path;
use std::process::Command;

const GUIDE: &str = include_str!("../../docs/self-hosting.md");

#[test]
fn guide_documents_the_delivered_deployment_boundary() {
    assert!(GUIDE.contains("## Deployment configuration"));
    assert!(GUIDE.contains("NAVIGATOR_DEPLOYMENTS_DIR"));
    assert!(!GUIDE.contains("Per-deployment configuration moves out of the source tree"));
    for limitation in ["SurrealDB (**Interim**)", "Restate (**Interim**)"] {
        assert!(
            GUIDE.contains(limitation),
            "missing limitation: {limitation}"
        );
    }
}

#[test]
fn documented_upgrade_plan_renders_without_credentials_or_external_tools() {
    let section = GUIDE
        .split("## Deployment configuration\n")
        .nth(1)
        .expect("guide documents the delivered external deployment root");
    let command = section
        .split("```bash\n")
        .nth(1)
        .and_then(|block| block.split("```").next())
        .expect("deployment section has a runnable offline example")
        .replace("\\\n", " ");
    let arguments = command
        .trim()
        .strip_prefix("cargo run -p cli -- ")
        .expect("example uses the source CLI");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CLI is in the repository");
    let empty = tempfile::tempdir().expect("empty environment directories");
    let args: Vec<_> = arguments
        .split_whitespace()
        .map(|arg| {
            if arg == "\"$PWD\"" {
                root.to_string_lossy().into_owned()
            } else {
                arg.to_owned()
            }
        })
        .collect();
    assert!(args.iter().any(|arg| arg == "--dry-run"));
    let output = Command::new(env!("CARGO_BIN_EXE_navigator"))
        .current_dir(empty.path())
        .env_clear()
        .env("HOME", empty.path())
        .env("PATH", empty.path())
        .args(args)
        .output()
        .expect("run the documented command using the built source CLI");
    let report = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{report}");
    assert!(report.contains("OFFLINE SHIP PLAN (roll)"), "{report}");
    assert!(
        report.contains("would: reconcile PostgreSQL StatefulSet"),
        "{report}"
    );
    assert!(
        report.contains("not checked: live authentication"),
        "{report}"
    );
    let count = report
        .split("parsed ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse::<usize>().ok())
        .expect("rendered manifest count");
    assert!(count > 0, "{report}");
}

#[test]
fn guide_points_at_the_forgejo_deployment_and_archive_boundary() {
    assert!(GUIDE.contains("\n## Forgejo\n"));
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CLI is in the repository");
    assert!(
        GUIDE.contains("(forgejo-deployment.md)"),
        "the Forgejo section links the deployment guide"
    );
    assert!(root.join("docs/forgejo-deployment.md").is_file());
    assert!(
        GUIDE.contains(
            "The Iceberg export covers the databases only; it does not cover Git repositories, LFS objects, or attachments."
        ),
        "the guide states the archive boundary"
    );
}

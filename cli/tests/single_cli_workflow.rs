//! Project release and forge workflows are available in the installed
//! Navigator executable, including on a clean install with no Hudson binary.

use assert_cmd::Command;

#[test]
fn navigator_discoveries_cover_project_release_and_forge_workflows() {
    for args in [
        vec!["github", "setup", "--help"],
        vec!["forgejo", "release", "mirror", "--help"],
        vec!["project", "repository", "repair-mirror", "--help"],
        vec!["project", "repository", "push", "--help"],
        vec!["project", "repository", "reconcile", "--help"],
        vec!["release", "default-tag", "--help"],
        vec!["release", "check", "--help"],
        vec!["release", "pins", "--help"],
        vec!["release", "review", "--help"],
        vec!["release", "version", "--help"],
    ] {
        Command::cargo_bin("navigator")
            .expect("Navigator binary is built for this integration test")
            .env("PATH", "")
            .args(args)
            .assert()
            .success();
    }
}

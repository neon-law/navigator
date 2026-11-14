//! The lean `navigator` carries Projects, their Jujutsu repositories, the
//! offline `validate` gate, the notation envelope verbs, the glossary, the site
//! session plus `seeds/` import a Project repository's CI runs, and the
//! matter-work `site` verbs (documents, mail filing, notations). Deployment
//! and Firm configuration live in `hudson`; Ephemeris in `scorpio`.

use assert_cmd::Command;

fn command_names(help: &str) -> Vec<String> {
    help.lines()
        .skip_while(|line| line.trim() != "Commands:")
        .skip(1)
        .take_while(|line| line.trim() != "Options:")
        .filter_map(|line| line.strip_prefix("  "))
        .filter(|row| !row.starts_with(char::is_whitespace))
        .filter_map(|row| row.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

fn help(args: &[&str]) -> String {
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .args(args)
        .output()
        .expect("run navigator help");
    assert!(output.status.success());
    String::from_utf8(output.stdout).expect("help output is utf-8")
}

#[test]
fn the_lean_navigator_names_only_its_own_verbs() {
    assert_eq!(
        command_names(&help(&["--help"])),
        ["glossary", "notation", "project", "site", "sync", "validate", "help"].map(String::from)
    );
    assert_eq!(
        command_names(&help(&["site", "--help"])),
        ["document", "import", "login", "logout", "mail", "notation", "whoami", "help"]
            .map(String::from)
    );
    assert_eq!(
        command_names(&help(&["notation", "envelope", "--help"])),
        ["prepare", "send", "status", "help"].map(String::from)
    );
}

#[test]
fn an_operator_verb_is_not_in_the_lean_navigator() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["ops", "--help"])
        .assert()
        .failure();
}

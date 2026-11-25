//! The lean `navigator` carries Projects, their Jujutsu repositories, the
//! offline `validate` gate, notation envelope verbs, the glossary, Calendar reads,
//! Relationship Log observations, a site session, and the `seeds/` import a Project's CI runs.
//! Matter-work `site` verbs handle documents, Authority filing, mail filing, and notations. Deployment
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
        [
            "billing",
            "calendar",
            "downloads",
            "firm",
            "forgejo",
            "glossary",
            "inbox",
            "jj",
            "mail",
            "notation",
            "project",
            "relationship",
            "release",
            "site",
            "sync",
            "validate",
            "help"
        ]
        .map(String::from)
    );
    assert_eq!(
        command_names(&help(&["site", "--help"])),
        [
            "authorities",
            "brand",
            "document",
            "firm",
            "import",
            "login",
            "logout",
            "mail",
            "notation",
            "person",
            "whoami",
            "help"
        ]
        .map(String::from)
    );
    assert_eq!(
        command_names(&help(&["relationship", "--help"])),
        ["add", "help"].map(String::from)
    );
    assert_eq!(
        command_names(&help(&["notation", "envelope", "--help"])),
        ["prepare", "send", "status", "help"].map(String::from)
    );
}

#[test]
fn sync_help_documents_the_checkout_directory_override() {
    assert!(help(&["sync", "--help"]).contains("--dir"));
}

#[test]
fn an_operator_verb_is_not_in_the_lean_navigator() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["ops", "--help"])
        .assert()
        .failure();
}

#[test]
fn mail_parse_help_states_the_document_lane_limits() {
    // LAW-470: the long help says where the bytes go and where the caps sit,
    // so an oversized refusal is legible before it happens.
    let long = help(&["mail", "parse", "--help"]);
    assert!(long.contains("25 MiB per document kind"), "{long}");
    assert!(long.contains("1 GiB"), "{long}");
    assert!(long.contains("object storage"), "{long}");
}

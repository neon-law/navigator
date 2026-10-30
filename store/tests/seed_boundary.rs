//! The seed boundary: what `store/seeds/` may ship.
//!
//! Every shipped seed is a public firm fact, nonidentifying reference data, or
//! conspicuously synthetic, and the synthetic layer is reachable only from a
//! `dev` profile. The inventory below is the machine-checked half of the table
//! in `docs/public-contributor-safety.md`; the contact-data scan is
//! `cli/tests/no_client_data.rs`.

use std::path::{Path, PathBuf};

use serde_yaml::Value;
use store::DeploymentEnvironment;

fn seeds_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds")
}

fn repo_file(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn records(rel: &str) -> Vec<Value> {
    let doc: Value = serde_yaml::from_str(&std::fs::read_to_string(seeds_dir().join(rel)).unwrap())
        .unwrap_or_else(|e| panic!("parse {rel}: {e}"));
    doc.get("records")
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default()
}

fn text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.clone(),
        other => panic!("{key} is not a string: {other:?}"),
    }
}

/// Seed files by relative path.
fn seed_files() -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let dir = seeds_dir();
    let mut out = Vec::new();
    walk(&dir, &dir, &mut out);
    out.sort();
    out
}

/// The canonical layer: reference catalogs plus the public firm facts.
const CANONICAL: &[&str] = &[
    "Credential.yaml",
    "Entity.yaml",
    "EntityType.yaml",
    "Jurisdiction.yaml",
    "Person.yaml",
    "PersonEntityRole.yaml",
    "Question.yaml",
    "Testimonial.yaml",
    "User.yaml",
];

/// The brand layer, applied to the brand's own deployment.
const NEON_BRAND: &[&str] = &[
    "neon/Address.yaml",
    "neon/Mailroom.yaml",
    "neon/Post.yaml",
    "neon/SocialHandle.yaml",
];

/// The synthetic layer, applied on a `dev` profile only.
const SAMPLE: &[&str] = &[
    "sample/Answer.yaml",
    "sample/GitRepository.yaml",
    "sample/Letter.yaml",
    "sample/Mailroom.yaml",
    "sample/PersonProjectRole.yaml",
];

/// A new seed file must be placed in a layer here, and so reviewed, before it
/// ships.
#[test]
fn every_seed_file_belongs_to_exactly_one_known_layer() {
    let mut expected: Vec<String> = CANONICAL
        .iter()
        .chain(NEON_BRAND)
        .chain(SAMPLE)
        .map(|p| (*p).to_string())
        .collect();
    expected.sort();
    assert_eq!(
        seed_files(),
        expected,
        "classify the new or removed seed file in a layer"
    );
}

/// Synthetic rows are labelled where a reader opens them.
#[test]
fn every_sample_seed_says_it_is_synthetic() {
    for file in SAMPLE {
        let body = std::fs::read_to_string(seeds_dir().join(file)).unwrap();
        assert!(
            body.lines().take(3).any(|l| l.starts_with("# Synthetic.")),
            "{file} must open with a `# Synthetic.` header"
        );
    }
}

/// No firm-layer seed borrows the synthetic vocabulary, and no sample seed
/// names a firm fact, so a row cannot cross layers unnoticed.
#[test]
fn layers_do_not_borrow_each_others_rows() {
    for file in CANONICAL.iter().chain(NEON_BRAND) {
        let body = std::fs::read_to_string(seeds_dir().join(file)).unwrap();
        assert!(
            !body.contains("Sample Mail Center") && !body.contains("sample-"),
            "{file} is shipped to production and must not name a sample row"
        );
    }
    for file in SAMPLE {
        let body = std::fs::read_to_string(seeds_dir().join(file)).unwrap();
        assert!(
            !body.contains("Ridgeview") && !body.contains("Mae Anne"),
            "{file} is synthetic and must not name the firm's real facility"
        );
    }
}

/// The identity seeds hold exactly the inventoried public firm facts, and each
/// fact appears in the provenance table.
#[test]
fn identity_seeds_hold_only_the_inventoried_public_firm_facts() {
    let doc = repo_file("docs/public-contributor-safety.md");

    let people = records("Person.yaml");
    assert_eq!(people.len(), 1);
    assert_eq!(text(&people[0], "email"), "nick@neonlaw.com");
    assert_eq!(text(&people[0], "name"), "Nick Shook");

    let users = records("User.yaml");
    assert_eq!(users.len(), 1);
    assert_eq!(text(&users[0], "role"), "admin");

    let entities = records("Entity.yaml");
    assert_eq!(entities.len(), 1);
    assert_eq!(text(&entities[0], "name"), store::seed::FIRM_ENTITY_NAME);

    let mut credentials: Vec<(String, String)> = records("Credential.yaml")
        .iter()
        .map(|c| (text(&c["jurisdiction"], "name"), text(c, "license_number")))
        .collect();
    credentials.sort();
    assert_eq!(
        credentials,
        [
            ("California", "337252"),
            ("Nevada", "13400"),
            ("New York", "6380059"),
            ("Washington", "63446")
        ]
        .map(|(j, n)| (j.to_string(), n.to_string())),
    );

    let boxes = records("neon/Address.yaml");
    assert_eq!(boxes.len(), 1);
    assert_eq!(text(&boxes[0], "street"), "5150 Mae Anne Ave Ste 405-9002");

    for fact in [
        "Guieb & Shook PC",
        "Nick Shook",
        "13400",
        "337252",
        "63446",
        "5150 Mae Anne Ave Ste 405-9002",
        "nick@neonlaw.com",
    ] {
        assert!(
            doc.contains(fact),
            "`{fact}` is seeded but has no provenance row in docs/public-contributor-safety.md"
        );
    }
    assert!(
        records("PersonEntityRole.yaml").is_empty() && records("Testimonial.yaml").is_empty(),
        "relationship and testimonial seeds are private runtime data"
    );
}

/// Unset or empty never means `dev`, and a typo is an error rather than a
/// fallback to either profile.
#[test]
fn no_implicit_configuration_selects_the_sample_layer() {
    for value in [None, Some(""), Some("production")] {
        let profile = DeploymentEnvironment::from_lookup(|_| value.map(str::to_owned)).unwrap();
        assert_eq!(profile, DeploymentEnvironment::Production, "{value:?}");
    }
    for typo in ["Dev", "development", "staging", "true"] {
        assert!(
            DeploymentEnvironment::from_lookup(|_| Some(typo.to_owned())).is_err(),
            "{typo} must be refused, not guessed"
        );
    }
}

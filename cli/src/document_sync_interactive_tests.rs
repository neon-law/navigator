//! `project sync` answered by a person: the per-item yes/no, `a`, `q`, and the
//! conflict prompt, driven in-process so a scripted prompter stands in for the
//! terminal the binary tests cannot allocate.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{reconcile_project, SyncMode};
use crate::document_reconcile::{Answer, Approval, Prompter, Side};
use crate::remote::DocumentClient;

const FILED: &[u8] = b"the filed bytes";
const NEWER: &[u8] = b"the newer live bytes";
const EDITED: &[u8] = b"the filed bytes, edited by a lawyer";

/// Plays back scripted answers and records every question it was asked.
struct Script {
    answers: Vec<char>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Script {
    fn next(&mut self, question: &str) -> char {
        self.asked.lock().unwrap().push(question.to_string());
        assert!(
            !self.answers.is_empty(),
            "asked an unscripted question: {question}"
        );
        self.answers.remove(0)
    }
}

impl Prompter for Script {
    fn confirm(&mut self, question: &str, offer_all: bool) -> Result<Answer> {
        Ok(match self.next(question) {
            'y' => Answer::Yes,
            'a' if offer_all => Answer::All,
            'q' => Answer::Quit,
            _ => Answer::No,
        })
    }

    fn choose_side(&mut self, question: &str, _offer_local: bool) -> Result<Side> {
        Ok(match self.next(question) {
            'l' => Side::Local,
            'r' => Side::Live,
            'q' => Side::Quit,
            _ => Side::Skip,
        })
    }
}

struct World {
    server: MockServer,
    root: tempfile::TempDir,
    project_id: Uuid,
}

fn sha(bytes: &[u8]) -> String {
    store::documents::sha256_hex(bytes)
}

fn live(id: Uuid, bytes: &[u8], slug: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "byte_size": bytes.len(), "sha256_hex": sha(bytes),
        "filename": "doc.pdf", "kind": "filing", "visibility": "internal",
        "slug": slug, "inserted_at": "2026-09-23T12:00:00Z"
    })
}

fn pointer(id: Uuid, bytes: &[u8], version: usize, previous: Option<Uuid>) -> String {
    let mut value = serde_json::json!({
        "kind": "filing", "visibility": "internal",
        "current_version": {
            "version": version, "asset_id": id,
            "created_at": "2026-09-23T12:00:00Z",
            "sha256": sha(bytes), "size_bytes": bytes.len()
        }
    });
    if let Some(previous) = previous {
        value["previous_version"] = previous.to_string().into();
    }
    serde_yaml::to_string(&value).unwrap()
}

fn put(root: &Path, relative: &str, bytes: impl AsRef<[u8]>) {
    let target = root.join(relative);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(target, bytes).unwrap();
}

async fn world(inventory: Vec<serde_json::Value>) -> World {
    let server = MockServer::start().await;
    let root = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root.path())
        .status()
        .unwrap()
        .success());
    let project_id = Uuid::now_v7();
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{project_id}/documents")))
        .respond_with(ResponseTemplate::new(200).set_body_json(inventory))
        .mount(&server)
        .await;
    World {
        server,
        root,
        project_id,
    }
}

impl World {
    async fn accept_uploads(&self, receipt_id: Uuid, bytes: &[u8], version: usize, previous: Uuid) {
        let body: serde_json::Value =
            serde_yaml::from_str(&pointer(receipt_id, bytes, version, Some(previous))).unwrap();
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{}/documents",
                self.project_id
            )))
            .respond_with(ResponseTemplate::new(201).set_body_json(body))
            .mount(&self.server)
            .await;
    }

    async fn serve(&self, id: Uuid, bytes: &[u8]) {
        Mock::given(method("GET"))
            .and(path(format!("/app/projects/acme/documents/{id}/download")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
            .mount(&self.server)
            .await;
    }

    async fn uploads(&self) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method == wiremock::http::Method::POST)
            .count()
    }

    /// The local bytes of `slug`, in whichever matter folder `project sync`
    /// put them, else at the slug itself (LAW-189).
    fn read(&self, slug: &str) -> Vec<u8> {
        let documents = self.root.path().join("documents");
        let path = store::document_pointers::MatterFolder::ALL
            .iter()
            .map(|folder| documents.join(folder.as_str()).join(slug))
            .find(|path| path.is_file())
            .unwrap_or_else(|| documents.join(slug));
        std::fs::read(path).unwrap()
    }

    async fn run(&self, answers: &str) -> (Result<()>, Vec<String>) {
        self.run_with(answers, false).await
    }

    async fn run_with(&self, answers: &str, yes_delete: bool) -> (Result<()>, Vec<String>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let script = Script {
            answers: answers.chars().collect(),
            asked: Arc::clone(&asked),
        };
        let client = DocumentClient::for_check_tests(self.server.uri(), self.project_id);
        let result = reconcile_project(
            self.root.path(),
            &client,
            SyncMode {
                dry_run: false,
                approval: Approval::Ask(Box::new(script)),
                yes_delete,
                scope: crate::document_sync::DocumentScope::all(),
            },
        )
        .await;
        let asked = asked.lock().unwrap().clone();
        (result, asked)
    }
}

/// Two edited documents on live pointers: each is asked on its own, a `n`
/// leaves it, a `y` uploads it, and the question carries the evidence.
#[tokio::test(flavor = "multi_thread")]
async fn each_edit_is_asked_individually_with_its_evidence() {
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let w = world(vec![
        live(a, FILED, "a/one.pdf"),
        live(b, FILED, "b/two.pdf"),
    ])
    .await;
    for slug in ["a/one.pdf", "b/two.pdf"] {
        put(w.root.path(), &format!("documents/{slug}"), EDITED);
    }
    put(
        w.root.path(),
        "blobs/a/one.pdf.yaml",
        pointer(a, FILED, 1, None),
    );
    put(
        w.root.path(),
        "blobs/b/two.pdf.yaml",
        pointer(b, FILED, 1, None),
    );
    w.accept_uploads(Uuid::now_v7(), EDITED, 2, b).await;

    let (result, asked) = w.run("ny").await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(asked.len(), 2, "one question per item");
    assert!(asked[0].contains("documents/a/one.pdf"), "{}", asked[0]);
    assert!(
        asked[0].contains(&sha(EDITED)[..8]),
        "local sha prefix: {}",
        asked[0]
    );
    assert!(
        asked[0].contains(&sha(FILED)[..8]),
        "live sha prefix: {}",
        asked[0]
    );
    assert!(asked[0].contains("revision 1"), "{}", asked[0]);
    assert_eq!(w.uploads().await, 1, "only the item answered yes");
}

#[tokio::test(flavor = "multi_thread")]
async fn all_answers_the_rest_and_quit_stops_asking() {
    let ids: Vec<Uuid> = (0..3).map(|_| Uuid::now_v7()).collect();
    let slugs = ["a/one.pdf", "b/two.pdf", "c/three.pdf"];
    let w = world(
        ids.iter()
            .zip(slugs)
            .map(|(id, slug)| live(*id, FILED, slug))
            .collect(),
    )
    .await;
    for (id, slug) in ids.iter().zip(slugs) {
        put(w.root.path(), &format!("documents/{slug}"), EDITED);
        put(
            w.root.path(),
            &format!("blobs/{slug}.yaml"),
            pointer(*id, FILED, 1, None),
        );
    }
    w.accept_uploads(Uuid::now_v7(), EDITED, 2, ids[0]).await;
    let (result, asked) = w.run("a").await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(asked.len(), 1, "`a` answers every remaining item");
    assert_eq!(w.uploads().await, 3);

    let quitting = world(vec![
        live(ids[0], FILED, "a/one.pdf"),
        live(ids[1], FILED, "b/two.pdf"),
    ])
    .await;
    for (id, slug) in ids.iter().zip(slugs).take(2) {
        put(quitting.root.path(), &format!("documents/{slug}"), EDITED);
        put(
            quitting.root.path(),
            &format!("blobs/{slug}.yaml"),
            pointer(*id, FILED, 1, None),
        );
    }
    let (result, asked) = quitting.run("q").await;
    assert!(
        result.is_ok(),
        "quitting is a choice, not a failure: {result:?}"
    );
    assert_eq!(asked.len(), 1);
    assert_eq!(quitting.uploads().await, 0);
}

/// A declined prompt leaves both sides alone, and the next run asks again.
#[tokio::test(flavor = "multi_thread")]
async fn a_declined_item_is_asked_again_on_the_next_run() {
    let id = Uuid::now_v7();
    let w = world(vec![live(id, FILED, "a/one.pdf")]).await;
    put(w.root.path(), "documents/a/one.pdf", EDITED);
    put(
        w.root.path(),
        "blobs/a/one.pdf.yaml",
        pointer(id, FILED, 1, None),
    );

    let (first, asked_first) = w.run("n").await;
    let (second, asked_second) = w.run("n").await;

    assert!(first.is_ok() && second.is_ok());
    assert_eq!(asked_first.len(), 1);
    assert_eq!(asked_second.len(), 1);
    assert_eq!(w.uploads().await, 0);
    assert_eq!(w.read("a/one.pdf"), EDITED);
}

/// The conflict prompt: local wins uploads a revision on top of the live one,
/// live wins replaces the local edits, skip leaves both.
#[tokio::test(flavor = "multi_thread")]
async fn a_conflict_takes_the_side_the_person_picks() {
    for (answer, uploads, local) in [('l', 1, EDITED), ('r', 0, NEWER), ('s', 0, EDITED)] {
        let (old, newer, receipt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let w = world(vec![
            live(newer, NEWER, "e/conflict.pdf"),
            live(old, FILED, "e/conflict.pdf"),
        ])
        .await;
        put(w.root.path(), "documents/e/conflict.pdf", EDITED);
        put(
            w.root.path(),
            "blobs/e/conflict.pdf.yaml",
            pointer(old, FILED, 1, None),
        );
        w.serve(newer, NEWER).await;
        w.accept_uploads(receipt, EDITED, 3, newer).await;

        let (result, asked) = w.run(&answer.to_string()).await;

        assert!(result.is_ok(), "{answer}: {result:?}");
        assert!(asked[0].contains("Conflict"), "{}", asked[0]);
        assert!(asked[0].contains("revision 2"), "{}", asked[0]);
        assert_eq!(w.uploads().await, uploads, "{answer}");
        assert_eq!(w.read("e/conflict.pdf"), local, "{answer}");
        let written = store::document_pointers::DocumentPointer::from_yaml(
            &std::fs::read_to_string(w.root.path().join("blobs/e/conflict.pdf.yaml")).unwrap(),
        )
        .unwrap();
        let expected = match answer {
            'l' => receipt,
            'r' => newer,
            _ => old,
        };
        assert_eq!(written.current_version.asset_id, expected, "{answer}");
    }
}

/// New files are asked once for the whole batch.
#[tokio::test(flavor = "multi_thread")]
async fn new_files_are_asked_once_for_the_batch() {
    let w = world(Vec::new()).await;
    put(w.root.path(), "documents/memos/one.pdf", b"one");
    put(w.root.path(), "documents/memos/two.pdf", b"two");
    w.accept_uploads(Uuid::now_v7(), b"one", 1, Uuid::now_v7())
        .await;

    let (declined, asked) = w.run("n").await;
    assert!(declined.is_ok());
    assert_eq!(asked.len(), 1, "one question for both files");
    assert!(asked[0].contains("memos/one.pdf") && asked[0].contains("memos/two.pdf"));
    assert_eq!(w.uploads().await, 0);
}

fn git_in(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=test@example.test"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// Commit and then remove the pointer for `slug`, as a lawyer deleting
/// `blobs/<slug>.yaml` would.
fn remove_pointer(w: &World, slug: &str, id: Uuid, visibility: &str) {
    let mut value: serde_json::Value = serde_yaml::from_str(&pointer(id, FILED, 1, None)).unwrap();
    value["visibility"] = visibility.into();
    put(
        w.root.path(),
        &format!("blobs/{slug}.yaml"),
        serde_yaml::to_string(&value).unwrap(),
    );
    git_in(w.root.path(), &["add", "blobs"]);
    git_in(
        w.root.path(),
        &["commit", "--quiet", "-m", "file the pointer"],
    );
    git_in(
        w.root.path(),
        &["rm", "--quiet", &format!("blobs/{slug}.yaml")],
    );
    git_in(
        w.root.path(),
        &["commit", "--quiet", "-m", "remove the pointer"],
    );
}

async fn accept_deletes(w: &World, ids: &[Uuid]) {
    for id in ids {
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{}/documents/{id}/delete",
                w.project_id
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "slug": "x", "revisions": 1, "changed": true,
                "deleted_at": "2026-10-02T12:00:00Z", "purge_after": "2027-03-31T12:00:00Z"
            })))
            .mount(&w.server)
            .await;
    }
}

async fn deletes(w: &World) -> usize {
    w.server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/delete"))
        .count()
}

/// Every deletion is its own question, `a` never answers one, and a yes to
/// one does not cover the next.
#[tokio::test(flavor = "multi_thread")]
async fn each_deletion_is_asked_on_its_own_and_all_never_covers_one() {
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let mut shared = live(b, FILED, "b/two.pdf");
    shared["visibility"] = "client".into();
    let w = world(vec![live(a, FILED, "a/one.pdf"), shared]).await;
    remove_pointer(&w, "a/one.pdf", a, "internal");
    remove_pointer(&w, "b/two.pdf", b, "client");
    accept_deletes(&w, &[a, b]).await;

    let (result, asked) = w.run("ya").await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(asked.len(), 2, "one question per deletion");
    assert_eq!(
        deletes(&w).await,
        1,
        "yes covers one deletion; `a` is not a deletion answer"
    );
    assert!(asked[0].contains("documents/a/one.pdf"), "{}", asked[0]);
    assert!(asked[0].contains("kind filing"), "{}", asked[0]);
    assert!(asked[0].contains("visibility internal"), "{}", asked[0]);
    assert!(asked[0].contains("1 revision(s)"), "{}", asked[0]);
    assert!(
        asked[0].contains(
            &(chrono::Utc::now() + chrono::Duration::days(180))
                .date_naive()
                .to_string()
        ),
        "the purge date: {}",
        asked[0]
    );
    assert!(!asked[0].contains("client will stop"), "{}", asked[0]);
    assert!(
        asked[1].contains("The client will stop seeing it."),
        "a client-visible document says so: {}",
        asked[1]
    );
}

/// Declining leaves the live document untouched, and the next run asks again.
#[tokio::test(flavor = "multi_thread")]
async fn a_declined_deletion_is_asked_again_on_the_next_run() {
    let id = Uuid::now_v7();
    let w = world(vec![live(id, FILED, "a/one.pdf")]).await;
    remove_pointer(&w, "a/one.pdf", id, "internal");
    accept_deletes(&w, &[id]).await;

    let (first, asked_first) = w.run("n").await;
    let (second, asked_second) = w.run("n").await;

    assert!(first.is_ok() && second.is_ok());
    assert_eq!(asked_first.len(), 1);
    assert_eq!(asked_second.len(), 1, "a declined deletion comes back");
    assert_eq!(deletes(&w).await, 0);
    let (third, _) = w.run("y").await;
    assert!(third.is_ok());
    assert_eq!(deletes(&w).await, 1);
}

/// `--yes-delete` answers a deletion without a prompt.
#[tokio::test(flavor = "multi_thread")]
async fn yes_delete_answers_a_deletion_without_asking() {
    let id = Uuid::now_v7();
    let w = world(vec![live(id, FILED, "a/one.pdf")]).await;
    remove_pointer(&w, "a/one.pdf", id, "internal");
    accept_deletes(&w, &[id]).await;

    let (result, asked) = w.run_with("", true).await;

    assert!(result.is_ok(), "{result:?}");
    assert!(asked.is_empty());
    assert_eq!(deletes(&w).await, 1);
}

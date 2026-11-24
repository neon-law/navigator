//! Run the wire exchange against Git's own upload-pack and receive-pack.
use super::*;
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use git2::{Oid, Repository, Signature};
use std::{
    collections::HashMap,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct Server {
    dir: PathBuf,
    race: Arc<Mutex<Option<Oid>>>,
}

fn git_rpc(server: &Server, repo: &str, service: &str, input: Option<Bytes>) -> Response {
    if !["primary", "mirror"].contains(&repo)
        || !["git-upload-pack", "git-receive-pack"].contains(&service)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = server.dir.join(repo);
    let mut command = Command::new("git");
    command
        .arg(service.trim_start_matches("git-"))
        .arg("--stateless-rpc");
    let mut result = Vec::new();
    let kind = if input.is_none() {
        command.arg("--advertise-refs");
        let label = format!("# service={service}\n");
        result.extend(format!("{:04x}{label}0000", label.len() + 4).bytes());
        "advertisement"
    } else {
        if repo == "mirror" && service == "git-receive-pack" {
            if let Some(id) = server.race.lock().unwrap().take() {
                Repository::open_bare(&path)
                    .unwrap()
                    .reference("refs/heads/main", id, true, "fixture race")
                    .unwrap();
            }
        }
        "result"
    };
    let mut child = command
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(bytes) = input {
        child.stdin.take().unwrap().write_all(&bytes).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    result.extend(out.stdout);
    (
        [(
            header::CONTENT_TYPE,
            format!("application/x-{service}-{kind}"),
        )],
        result,
    )
        .into_response()
}

async fn advertise(
    State(s): State<Server>,
    Path(repo): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    assert_eq!(headers[header::AUTHORIZATION], "Basic dTpw");
    git_rpc(&s, &repo, &q["service"], None)
}

async fn rpc(
    State(s): State<Server>,
    Path((repo, service)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    assert_eq!(headers[header::AUTHORIZATION], "Basic dTpw");
    git_rpc(&s, &repo, &service, Some(body))
}

fn commit(repo: &Repository, parent: Option<Oid>, message: &str) -> Oid {
    let mut builder = repo.treebuilder(None).unwrap();
    builder
        .insert("note", repo.blob(message.as_bytes()).unwrap(), 0o100_644)
        .unwrap();
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let sig = Signature::now("Fixture", "fixture@example.invalid").unwrap();
    let previous = parent.map(|id| repo.find_commit(id).unwrap());
    repo.commit(
        Some("refs/heads/main"),
        &sig,
        &sig,
        message,
        &tree,
        &previous.iter().collect::<Vec<_>>(),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_servers_enforce_ancestry_and_compare_the_advertised_old_ref() {
    let dir = tempfile::tempdir().unwrap();
    let primary = Repository::init_bare(dir.path().join("primary")).unwrap();
    primary.set_head("refs/heads/main").unwrap();
    let base = commit(&primary, None, "base");
    let mirror = git2::build::RepoBuilder::new()
        .bare(true)
        .clone(primary.path().to_str().unwrap(), &dir.path().join("mirror"))
        .unwrap();
    let ahead = commit(&primary, Some(base), "advance");
    let fork = commit(&mirror, Some(base), "independent");
    mirror
        .reference("refs/heads/main", base, true, "fixture")
        .unwrap();
    let race = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/{repo}/info/refs", get(advertise))
        .route("/{repo}/{service}", post(rpc))
        .with_state(Server {
            dir: dir.path().to_owned(),
            race: race.clone(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        let access = |repo| Access {
            url: format!("http://{address}/{repo}"),
            user: "u".into(),
            password: "p".into(),
        };
        let sync = |old| synchronize(&access("primary"), &access("mirror"), ahead, old);
        *race.lock().unwrap() = Some(fork);
        assert!(sync(base).is_err());
        assert_eq!(mirror.refname_to_id("refs/heads/main").unwrap(), fork);
        assert_eq!(sync(fork).unwrap(), SyncResult::Diverged);
        mirror
            .reference("refs/heads/main", base, true, "fixture")
            .unwrap();
        assert_eq!(sync(base).unwrap(), SyncResult::Advanced);
        assert_eq!(
            mirror
                .find_commit(ahead)
                .unwrap()
                .tree()
                .unwrap()
                .get_name("note")
                .unwrap()
                .id(),
            mirror.blob(b"advance").unwrap()
        );
        assert_eq!(mirror.refname_to_id("refs/heads/main").unwrap(), ahead);
        assert_eq!(sync(base).unwrap(), SyncResult::ChangedDuringSync);
    })
    .await
    .unwrap();
    server.abort();
}

//! Read the regular Git blob reachable at a full commit in a verified private repository.
use super::{ForgeError, ForgejoForge};
use base64::Engine as _;
use model::deadline_proposals::ForgejoPin;
use reqwest::{redirect::Policy, Client, Url};
use serde::{de::DeserializeOwned, Deserialize};
use sha1::{Digest as _, Sha1};

const METADATA_LIMIT: usize = 2 * 1024 * 1024;
const MAX_BLOB: usize = 32 * 1024 * 1024;
const ACTION: &str = "reading a pinned citation";
const INVALID: ForgeError = ForgeError::InvalidConfig("Forgejo citation");

pub enum CitationRepository<'a> {
    Named(&'a str),
    Recorded { origin: &'a str, id: i64 },
}

pub struct CitationFile {
    pub origin: String,
    pub name: String,
    pub repository_id: i64,
    pub browser_url: Url,
    pub bytes: Vec<u8>,
}

#[derive(Deserialize)]
struct Repository {
    id: i64,
    name: String,
    private: bool,
    owner: Owner,
}

#[derive(Deserialize)]
struct Owner {
    login: String,
}

#[derive(Deserialize)]
struct Object {
    sha: String,
}

#[derive(Deserialize)]
struct Commit {
    sha: String,
    commit: CommitTree,
}

#[derive(Deserialize)]
struct CommitTree {
    tree: Object,
}

#[derive(Deserialize)]
struct Tree {
    sha: String,
    #[serde(rename = "tree")]
    entries: Option<Vec<Entry>>,
    truncated: bool,
}

#[derive(Deserialize)]
struct Entry {
    sha: String,
    mode: String,
    path: String,
}

#[derive(Deserialize)]
struct Blob {
    sha: String,
    size: usize,
    encoding: String,
    content: String,
}

async fn json<T: DeserializeOwned>(
    forge: &ForgejoForge,
    http: &Client,
    url: String,
    limit: usize,
) -> Result<T, ForgeError> {
    let mut response = ForgejoForge::checked(forge.request(http.get(url)).send().await, ACTION)?;
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(INVALID);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|source| ForgeError::Response {
            action: ACTION,
            source,
        })?
    {
        if chunk.len() > limit - body.len() {
            return Err(INVALID);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| INVALID)
}

fn is_object_id(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

async fn committed_bytes(
    forge: &ForgejoForge,
    http: &Client,
    repo_url: &str,
    pin: &ForgejoPin,
) -> Result<Vec<u8>, ForgeError> {
    let tip: Commit = json(
        forge,
        http,
        format!(
            "{repo_url}/git/commits/{}?stat=false&files=false",
            pin.commit
        ),
        METADATA_LIMIT,
    )
    .await?;
    if tip.sha != pin.commit {
        return Err(INVALID);
    }
    let mut object = tip.commit.tree.sha;
    let mut parts = pin.path.split('/').peekable();
    while let Some(part) = parts.next() {
        if !is_object_id(&object) {
            return Err(INVALID);
        }
        let mut page = 1_u32;
        let entry = loop {
            let tree: Tree = json(
                forge,
                http,
                format!("{repo_url}/git/trees/{object}?page={page}"),
                METADATA_LIMIT,
            )
            .await?;
            if tree.sha != object {
                return Err(INVALID);
            }
            if let Some(entry) = tree.entries.into_iter().flatten().find(|e| e.path == part) {
                break entry;
            }
            if !tree.truncated {
                return Err(ForgeError::Api {
                    action: ACTION,
                    status: 404,
                });
            }
            page = page.checked_add(1).ok_or(INVALID)?;
        };
        let allowed = if parts.peek().is_some() {
            entry.mode == "040000"
        } else {
            matches!(entry.mode.as_str(), "100644" | "100755")
        };
        if !allowed {
            return Err(INVALID);
        }
        object = entry.sha;
    }
    if !is_object_id(&object) {
        return Err(INVALID);
    }
    let blob: Blob = json(
        forge,
        http,
        format!("{repo_url}/git/blobs/{object}"),
        MAX_BLOB.div_ceil(3) * 4 + METADATA_LIMIT,
    )
    .await?;
    if blob.sha != object || blob.encoding != "base64" || blob.size > MAX_BLOB {
        return Err(INVALID);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(blob.content)
        .map_err(|_| INVALID)?;
    // Git's object ID covers its blob header and bytes. The archived original has a separate SHA-256.
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", bytes.len()));
    hash.update(&bytes);
    if bytes.len() != blob.size || hex::encode(hash.finalize()) != object {
        return Err(INVALID);
    }
    Ok(bytes)
}

impl ForgejoForge {
    /// Resolve the recorded commit to its tree, then follow regular tree entries to the blob.
    /// A historical repository is located by its numeric ID on the original installation.
    pub async fn citation_file(
        &self,
        repository: CitationRepository<'_>,
        pin: &ForgejoPin,
    ) -> Result<CitationFile, ForgeError> {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.fetch_citation_file(repository, pin),
        )
        .await
        .map_err(|_| ForgeError::Api {
            action: ACTION,
            status: 504,
        })?
    }

    async fn fetch_citation_file(
        &self,
        repository: CitationRepository<'_>,
        pin: &ForgejoPin,
    ) -> Result<CitationFile, ForgeError> {
        pin.validate().map_err(|_| INVALID)?;
        let mut root = Url::parse(&self.root).map_err(|_| INVALID)?;
        let secure = root.scheme() == "https"
            || (root.scheme() == "http"
                && root.host_str().is_some_and(|h| {
                    h == "localhost"
                        || h.parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback())
                        || h.ends_with(".svc.cluster.local")
                }));
        if !secure {
            return Err(INVALID);
        }
        if root.host_str().is_none()
            || !root.username().is_empty()
            || root.password().is_some()
            || root.query().is_some()
            || root.fragment().is_some()
        {
            return Err(INVALID);
        }
        let origin = root.as_str().trim_end_matches('/').to_owned();
        let lookup = match repository {
            CitationRepository::Named(name) => self.repo_url(name)?,
            CitationRepository::Recorded { origin: saved, id } if origin == saved && id > 0 => {
                format!("{}/repositories/{id}", self.api_base)
            }
            CitationRepository::Recorded { .. } => return Err(INVALID),
        };
        let http = Client::builder()
            .redirect(Policy::none())
            .build()
            .map_err(|source| ForgeError::Request {
                action: ACTION,
                source,
            })?;
        let repo: Repository = json(self, &http, lookup, METADATA_LIMIT).await?;
        if !repo.private
            || repo.owner.login != self.owner
            || repo.id <= 0
            || !crate::workspace::is_valid_slug(&repo.name)
            || match repository {
                CitationRepository::Named(name) => repo.name != name,
                CitationRepository::Recorded { id, .. } => repo.id != id,
            }
        {
            return Err(INVALID);
        }
        let repo_url = self.repo_url(&repo.name)?;
        let bytes = committed_bytes(self, &http, &repo_url, pin).await?;
        // A namespace transfer or replacement during the series of reads invalidates the result.
        let after: Repository = json(self, &http, repo_url, METADATA_LIMIT).await?;
        if !after.private
            || after.id != repo.id
            || after.owner.login != self.owner
            || after.name != repo.name
        {
            return Err(INVALID);
        }
        root.path_segments_mut()
            .map_err(|()| INVALID)?
            .pop_if_empty()
            .extend([&self.owner, &repo.name, "src", "commit", &pin.commit])
            .extend(pin.path.split('/'));
        Ok(CitationFile {
            origin,
            name: repo.name,
            repository_id: repo.id,
            browser_url: root,
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::deadline_proposals::ForgejoRepository;
    use serde_json::{json, Value};
    use wiremock::{
        matchers::{header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    const REPO: &str = "/forge/api/v1/repos/fixture/renamed";
    fn metadata(id: i64, owner: &str) -> Value {
        json!({"id":id,"name":"renamed","private":true,"owner":{"login":owner},"html_url":"https://foreign.invalid/untrusted"})
    }
    async fn response(s: &MockServer, route: &str, value: Value) {
        Mock::given(method("GET"))
            .and(path(route))
            .and(header("Authorization", "token fixture-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(value))
            .mount(s)
            .await;
    }
    fn pin() -> ForgejoPin {
        ForgejoPin {
            repository: ForgejoRepository::Project,
            commit: "a".repeat(40),
            path: "Court files/order.pdf".into(),
        }
    }
    async fn tree(s: &MockServer, pin: &ForgejoPin, mode: &str) {
        let (root, sub, blob) = (
            "b".repeat(40),
            "c".repeat(40),
            "4215fa43adb196722f589dbd3bf530cd36c568a4",
        );
        response(
            s,
            &format!("{REPO}/git/commits/{}", pin.commit),
            json!({"sha":pin.commit,"commit":{"tree":{"sha":root}}}),
        )
        .await;
        response(s,&format!("{REPO}/git/trees/{root}"),json!({"sha":root,"truncated":false,"tree":[{"path":"Court files","mode":"040000","sha":sub}]})).await;
        response(s,&format!("{REPO}/git/trees/{sub}"),json!({"sha":sub,"truncated":false,"tree":[{"path":"order.pdf","mode":mode,"sha":blob}]})).await;
        response(
            s,
            &format!("{REPO}/git/blobs/{blob}"),
            json!({"sha":blob,"size":5,"encoding":"base64","content":"cHJvb2Y="}),
        )
        .await;
    }
    #[tokio::test]
    async fn original_repository_id_opens_at_the_pinned_commit_after_a_rename() {
        let s = MockServer::start().await;
        let origin = format!("{}/forge", s.uri());
        let forge = ForgejoForge::new(&origin, "fixture", "fixture-secret").unwrap();
        let pin = pin();
        response(&s, "/forge/api/v1/repositories/17", metadata(17, "fixture")).await;
        response(&s, REPO, metadata(17, "fixture")).await;
        tree(&s, &pin, "100644").await;
        for target in [
            CitationRepository::Named("renamed"),
            CitationRepository::Recorded {
                origin: &origin,
                id: 17,
            },
        ] {
            let file = forge.citation_file(target, &pin).await.unwrap();
            assert_eq!(file.bytes, b"proof");
            assert_eq!(file.repository_id, 17);
            assert_eq!(
                file.browser_url.as_str(),
                format!(
                    "{origin}/fixture/renamed/src/commit/{}/Court%20files/order.pdf",
                    pin.commit
                )
            );
        }
    }
    #[tokio::test]
    async fn a_blob_must_hash_to_the_object_in_the_commit_tree() {
        let server = MockServer::start().await;
        let origin = format!("{}/forge", server.uri());
        let forge = ForgejoForge::new(&origin, "fixture", "fixture-secret").unwrap();
        response(&server, REPO, metadata(17, "fixture")).await;
        let pin = pin();
        tree(&server, &pin, "100644").await;
        let blob = "4215fa43adb196722f589dbd3bf530cd36c568a4";
        Mock::given(method("GET"))
            .and(path(format!("{REPO}/git/blobs/{blob}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "sha":blob, "size":5, "encoding":"base64", "content":"ZmFsc2U="
            })))
            .with_priority(1)
            .mount(&server)
            .await;
        assert!(matches!(
            forge
                .citation_file(CitationRepository::Named("renamed"), &pin)
                .await,
            Err(ForgeError::InvalidConfig(_))
        ));
    }
    #[tokio::test]
    async fn a_changed_firm_repository_id_or_installation_cannot_reopen_a_pin() {
        let s = MockServer::start().await;
        let origin = format!("{}/forge", s.uri());
        let pin = pin();
        let forge = ForgejoForge::new(&origin, "fixture", "fixture-secret").unwrap();
        for (recorded_id, reply) in [
            (17, metadata(17, "someone-else")),
            (18, metadata(19, "fixture")),
        ] {
            response(
                &s,
                &format!("/forge/api/v1/repositories/{recorded_id}"),
                reply,
            )
            .await;
            assert!(forge
                .citation_file(
                    CitationRepository::Recorded {
                        origin: &origin,
                        id: recorded_id
                    },
                    &pin
                )
                .await
                .is_err());
        }
        assert!(forge
            .citation_file(
                CitationRepository::Recorded {
                    origin: "https://other.invalid",
                    id: 17
                },
                &pin
            )
            .await
            .is_err());
        assert_eq!(s.received_requests().await.unwrap().len(), 2);
    }
    #[tokio::test]
    async fn symlinks_and_replacements_during_a_read_are_refused() {
        let s = MockServer::start().await;
        let origin = format!("{}/forge", s.uri());
        let pin = pin();
        let forge = ForgejoForge::new(&origin, "fixture", "fixture-secret").unwrap();
        for mode in ["120000", "100644"] {
            s.reset().await;
            response(&s, "/forge/api/v1/repositories/17", metadata(17, "fixture")).await;
            response(&s, REPO, metadata(18, "fixture")).await;
            tree(&s, &pin, mode).await;
            assert!(forge
                .citation_file(
                    CitationRepository::Recorded {
                        origin: &origin,
                        id: 17
                    },
                    &pin
                )
                .await
                .is_err());
        }
        s.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&s)
            .await;
        assert!(matches!(
            forge
                .citation_file(CitationRepository::Named("renamed"), &pin)
                .await,
            Err(ForgeError::Api { status: 503, .. })
        ));
    }
}

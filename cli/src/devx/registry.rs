//! Shared container-registry helpers.
//!
//! CI (`deploy.yml`) builds and pushes every navigator image to the registry
//! configured by `NAVIGATOR_IMAGE_REGISTRY`, tagged with an immutable release
//! name. Three callers resolve and verify those tags and must
//! do it identically, so the logic lives here once:
//!
//! - `ship` — rolls **prod** onto a published tag.
//! - `deploy` / `up` — pull the published images into the **local KIND**
//!   cluster instead of building them on the host.
//! - `worktree_env --demo` — pulls the full stack into a per-worktree
//!   demo cluster.
//!
//! Authentication and retention are properties of the selected registry.
//!
//! A fork that sets `NAVIGATOR_IMAGE_REGISTRY` to a public registry such as
//! `ghcr.io/<owner>` still resolves tags anonymously: those registries mint a
//! pull token for a public package to anyone who asks.

use anyhow::{bail, Context, Result};

/// Resolve the registry prefix from the environment.
///
/// One variable, where the GAR path needed three — a region, a hub project,
/// and a repository name, any two of which could disagree and produce a
/// syntactically valid reference to somewhere no image had ever been pushed.
pub fn registry_from_env() -> Result<String> {
    registry_from_value(std::env::var("NAVIGATOR_IMAGE_REGISTRY").ok())
}

fn registry_from_value(value: Option<String>) -> Result<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("NAVIGATOR_IMAGE_REGISTRY must be set to <host>/<namespace>")
        })
}

/// Full image reference `<registry>/<image>:<tag>`.
#[must_use]
pub fn image_ref(registry: &str, image: &str, tag: &str) -> String {
    format!("{registry}/{image}:{tag}")
}

/// True when `tag` names an immutable release.
///
/// The grammar is semver's, via [`crate::release`], so `YY.M.D-hotfix.N` ranks
/// below the `YY.M.D` it hangs off, as Cargo and every consumer resolving a
/// version compute. One implementation answers this for the release pipeline
/// and for `ops ship` alike.
///
/// A four-component `YY.M.D.H` tag is refused: Cargo cannot parse it as a
/// version, and the tag comes from `[workspace.package].version`.
#[must_use]
pub fn is_release_tag(tag: &str) -> bool {
    crate::release::is_release_version(tag)
}

/// Reject a `--tag` that is not an immutable release tag — rolling `latest`,
/// `buildcache`, or `ci-<sha>` onto a workload is exactly the unauditable
/// deploy we forbid.
pub fn validate_release_tag(tag: &str) -> Result<()> {
    crate::release::parse(tag)
        .map(|_| ())
        .with_context(|| format!("--tag must be an immutable release tag, got `{tag}`"))
}

/// The newest immutable release tag in `tags`.
///
/// Ordering is semver's, so a base version outranks its own prerelease and
/// `26.8.22` is correctly newer than `26.8.22-hotfix.22`. Non-release tags are
/// ignored.
#[must_use]
pub fn pick_latest_release_tag(tags: &[String]) -> Option<String> {
    let latest = crate::release::highest_release(tags)?;
    // Return the tag as the registry spells it rather than the re-rendered
    // version: the string is about to be used as an image reference, and a
    // round-trip through `Version` is a chance to hand back something the
    // registry does not have.
    tags.iter()
        .find(|tag| crate::release::parse(tag).ok().as_ref() == Some(&latest))
        .cloned()
}

/// Resolve the latest published immutable release tag for `<registry>/<image>`.
/// Errors when the package has no release tag yet — a fork whose `main` has
/// never carried a version bump has published nothing.
pub fn resolve_latest_tag(registry: &str, image: &str) -> Result<String> {
    let tags = fetch_tags(registry, image)?;
    pick_latest_release_tag(&tags).ok_or_else(|| {
        anyhow::anyhow!(
            "no release tag on {registry}/{image} — has a version bump landed on main yet?"
        )
    })
}

/// Whether `host` is an Artifact Registry Docker host (`<region>-docker.pkg.dev`).
fn is_artifact_registry(host: &str) -> bool {
    host.ends_with("-docker.pkg.dev")
}

/// List an image's tags via the Docker Registry v2 API at
/// `https://<host>/v2/<namespace>/<image>/tags/list`.
///
/// Builds a private current-thread runtime so callers stay sync.
///
/// Authentication depends on the registry. Artifact Registry is private, so the
/// caller's Google access token is sent as the bearer directly. Any other
/// registry (a fork's public `ghcr.io/<owner>`) is asked for an anonymous pull
/// token first, which it mints for a public package to anyone who asks.
pub fn fetch_tags(registry: &str, image: &str) -> Result<Vec<String>> {
    let (host, namespace) = registry.split_once('/').with_context(|| {
        format!("registry `{registry}` is not `<host>/<namespace>` — cannot list tags")
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for registry tag resolution")?;
    let repository = format!("{namespace}/{image}");
    let token_url =
        format!("https://{host}/token?scope=repository:{repository}:pull&service={host}");
    let list_url = format!("https://{host}/v2/{repository}/tags/list");
    runtime.block_on(async move {
        let client = reqwest::Client::new();
        let token: String = if is_artifact_registry(host) {
            super::gcp::auth::adc_token_provider()
                .await
                .context("acquire a Google credential to read the private image registry")?
                .token()
                .await
                .context("mint a Google access token for the image registry")?
        } else {
            client
                .get(&token_url)
                .send()
                .await
                .context("request registry pull token")?
                .json::<serde_json::Value>()
                .await
                .context("parse registry token response")?
                .get("token")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .with_context(|| format!("{host} returned no pull token for {repository}"))?
        };
        let resp = client
            .get(&list_url)
            .bearer_auth(token)
            .send()
            .await
            .context("request registry tags/list")?;
        if !resp.status().is_success() {
            bail!(
                "tags/list for {repository} returned {} — is the image published, and does the \
                 caller hold `roles/artifactregistry.reader` on {registry}? Images there are \
                 deleted 7 days after push, so a tag older than a week is gone.",
                resp.status()
            );
        }
        let body: serde_json::Value = resp.json().await.context("parse registry tags/list")?;
        let tags = body
            .get("tags")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        Ok(tags)
    })
}

/// Whether `tag` is published for `<registry>/<image>`.
pub fn tag_exists(registry: &str, image: &str, tag: &str) -> Result<bool> {
    Ok(fetch_tags(registry, image)?.iter().any(|t| t == tag))
}

/// Resolve an immutable image tag to the registry's canonical manifest digest.
pub fn tag_digest(registry: &str, image: &str, tag: &str) -> Result<String> {
    let (host, namespace) = registry.split_once('/').with_context(|| {
        format!("registry `{registry}` is not `<host>/<namespace>` — cannot resolve digest")
    })?;
    let repository = format!("{namespace}/{image}");
    let token_url =
        format!("https://{host}/token?scope=repository:{repository}:pull&service={host}");
    let manifest_url = format!("https://{host}/v2/{repository}/manifests/{tag}");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for registry digest lookup")?;
    runtime.block_on(tag_digest_from_urls(
        host,
        &repository,
        tag,
        &token_url,
        &manifest_url,
    ))
}

async fn tag_digest_from_urls(
    host: &str,
    repository: &str,
    tag: &str,
    token_url: &str,
    manifest_url: &str,
) -> Result<String> {
    let client = reqwest::Client::new();
    let token: String = if is_artifact_registry(host) {
        super::gcp::auth::adc_token_provider()
            .await
            .context("acquire a Google credential to read the private image registry")?
            .token()
            .await
            .context("mint a Google access token for the image registry")?
    } else {
        client
            .get(token_url)
            .send()
            .await
            .context("request registry pull token")?
            .json::<serde_json::Value>()
            .await
            .context("parse registry token response")?
            .get("token")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .with_context(|| format!("{host} returned no pull token for {repository}"))?
    };
    let response = client
        .get(manifest_url)
        .bearer_auth(token)
        .header(
            reqwest::header::ACCEPT,
            "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json",
        )
        .send()
        .await
        .context("request registry image manifest")?;
    if !response.status().is_success() {
        bail!(
            "manifest for {repository}:{tag} returned {}",
            response.status()
        );
    }
    response
        .headers()
        .get("docker-content-digest")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .with_context(|| format!("registry returned no canonical digest for {repository}:{tag}"))
}

/// Bail unless `tag` is published for `<registry>/<image>`. Used to fail
/// fast — before any `kubectl set image` / `docker pull` — when an image
/// is missing the requested tag (which would otherwise wedge a deployment
/// in `ImagePullBackOff`). Distinguishes a lookup error (network / auth)
/// from an honestly-absent tag.
pub fn ensure_tag_published(registry: &str, image: &str, tag: &str) -> Result<()> {
    let tags = fetch_tags(registry, image)
        .with_context(|| format!("check {registry}/{image}:{tag} is published"))?;
    if tags.iter().any(|t| t == tag) {
        Ok(())
    } else {
        bail!(
            "{registry}/{image}:{tag} is not published — images are deleted 7 days after push, so \
             a tag older than a week is gone. Pick a newer tag, or republish from source."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_REGISTRY: &str = "ghcr.io/example/navigator";

    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn tag_digest_uses_public_pull_token_and_returns_canonical_digest() {
        let server = MockServer::start().await;
        let repository = "team/navigator-web";
        let tag = "26.10.12";
        Mock::given(method("GET"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "pull-token"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/{repository}/manifests/{tag}")))
            .and(header("authorization", "Bearer pull-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("docker-content-digest", "sha256:canonical")
                    .set_body_string("{}"),
            )
            .mount(&server)
            .await;

        assert_eq!(
            tag_digest_from_urls(
                "registry.example",
                repository,
                tag,
                &format!("{}/token", server.uri()),
                &format!("{}/v2/{repository}/manifests/{tag}", server.uri()),
            )
            .await
            .unwrap(),
            "sha256:canonical"
        );
    }

    #[tokio::test]
    async fn tag_digest_reports_missing_pull_token_and_manifest_digest() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        let repository = "team/navigator-web";
        let tag = "26.10.12";
        let token_error = tag_digest_from_urls(
            "registry.example",
            repository,
            tag,
            &format!("{}/token", server.uri()),
            &format!("{}/v2/{repository}/manifests/{tag}", server.uri()),
        )
        .await
        .unwrap_err();
        assert!(token_error.to_string().contains("returned no pull token"));

        let missing_digest_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "pull-token"
            })))
            .mount(&missing_digest_server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/{repository}/manifests/{tag}")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&missing_digest_server)
            .await;
        let digest_error = tag_digest_from_urls(
            "registry.example",
            repository,
            tag,
            &format!("{}/token", missing_digest_server.uri()),
            &format!(
                "{}/v2/{repository}/manifests/{tag}",
                missing_digest_server.uri()
            ),
        )
        .await
        .unwrap_err();
        assert!(digest_error.to_string().contains("no canonical digest"));
    }

    #[tokio::test]
    async fn tag_digest_rejects_an_unpublished_manifest() {
        let server = MockServer::start().await;
        let repository = "team/navigator-web";
        let tag = "26.10.12";
        Mock::given(method("GET"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "pull-token"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/{repository}/manifests/{tag}")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let error = tag_digest_from_urls(
            "registry.example",
            repository,
            tag,
            &format!("{}/token", server.uri()),
            &format!("{}/v2/{repository}/manifests/{tag}", server.uri()),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("returned 404"));
    }

    /// What `--tag` admits is a release VERSION, which is now the same question
    /// the release pipeline asks. A rolling or per-commit ref is what this must
    /// refuse: rolling `latest` onto a workload is the unauditable deploy the
    /// whole immutable-tag convention exists to prevent.
    #[test]
    fn is_release_tag_accepts_a_release_version_and_nothing_else() {
        assert!(is_release_tag("26.6.23"));
        assert!(is_release_tag("26.6.5")); // single-digit day
        assert!(is_release_tag("0.1.9"));
        assert!(is_release_tag("26.8.19-hotfix.14"));
        assert!(is_release_tag("26.8.19-hotfix.0"));
        assert!(is_release_tag("26.8.19-hotfix.214"));
        // The calendar is a convention, so a version departing from it is still
        // a version: a four-digit year is accepted.
        assert!(is_release_tag("2026.6.23"));
        // Non-releases and malformed shapes stay rejected.
        assert!(!is_release_tag("latest"));
        assert!(!is_release_tag("buildcache"));
        assert!(!is_release_tag("ci-6a5f96a"));
        assert!(!is_release_tag("26.6")); // too few components
        assert!(!is_release_tag("26..6")); // empty component
        assert!(!is_release_tag("26.8.19-hotfix.")); // empty prerelease identifier
        assert!(!is_release_tag("26.8.19-hotfix.014")); // a numeric identifier may not pad
                                                        // A bare alphanumeric prerelease IS a version, and it orders below any
                                                        // numbered one (`hotfix` < `hotfix.14`), so nothing needs to refuse it.
                                                        // The old grammar did, for requiring `-hotfix.N` exactly.
        assert!(is_release_tag("26.8.19-hotfix"));
        assert!(is_release_tag("26.8.19-hotfix.14.1"));
        // A fourth component is not a version Cargo can parse, so no release
        // can carry one — the legacy `.H` spelling is retired with the
        // derived-tag era that produced it.
        assert!(!is_release_tag("26.6.25.14"));
        assert!(!is_release_tag("26.6.25.14.30"));
        // Build metadata does not order, so two tags differing only there would
        // compare equal.
        assert!(!is_release_tag("26.6.23+build.1"));
    }

    #[test]
    fn pick_latest_release_tag_takes_the_newest_and_ignores_non_releases() {
        let tags = vec![
            "latest".to_string(),
            "26.6.10".to_string(),
            "ci-deadbeef".to_string(),
            "26.6.23".to_string(),
            "26.5.31".to_string(),
        ];
        assert_eq!(pick_latest_release_tag(&tags), Some("26.6.23".to_string()));
        // A BASE VERSION OUTRANKS ITS OWN PRERELEASE. This is the assertion that
        // reversed: the retired hand-rolled ordering put `-hotfix.N` above the
        // version it hangs off, so `ops ship` resolving "latest" would roll the
        // hotfix over the release that superseded it. Semver — and therefore
        // Cargo, and therefore every consumer — orders it the other way.
        assert_eq!(
            pick_latest_release_tag(&[
                "26.8.19-hotfix.9".to_string(),
                "26.8.19".to_string(),
                "26.8.19-hotfix.14".to_string(),
            ]),
            Some("26.8.19".to_string())
        );
        // Among prereleases of one base, the larger number wins.
        assert_eq!(
            pick_latest_release_tag(&[
                "26.8.19-hotfix.9".to_string(),
                "26.8.19-hotfix.14".to_string(),
            ]),
            Some("26.8.19-hotfix.14".to_string())
        );
        // Regression: numeric, not lexical, ordering. A plain string `max`
        // would pick `26.6.5` over `26.6.30` ("5" > "3") and `26.6.x` over
        // `26.10.x` ("6" > "1") — both chronologically wrong.
        assert_eq!(
            pick_latest_release_tag(&[
                "26.6.5".to_string(),
                "26.6.30".to_string(),
                "26.10.5".to_string(),
            ]),
            Some("26.10.5".to_string())
        );
        // A later month wins even though its day is smaller.
        assert_eq!(
            pick_latest_release_tag(&["26.6.30".to_string(), "26.7.1".to_string()]),
            Some("26.7.1".to_string())
        );
        assert_eq!(
            pick_latest_release_tag(&["latest".to_string(), "ci-x".to_string()]),
            None
        );
    }

    #[test]
    fn validate_release_tag_rejects_non_release() {
        assert!(validate_release_tag("26.6.23").is_ok());
        assert!(validate_release_tag("26.8.19-hotfix.14").is_ok());
        assert!(validate_release_tag("latest").is_err());
        assert!(validate_release_tag("buildcache").is_err());
        assert!(validate_release_tag("ci-abc").is_err());
    }

    #[test]
    fn image_ref_composes_the_published_path() {
        let reg = TEST_REGISTRY;
        assert_eq!(
            image_ref(reg, "navigator-web", "26.6.23"),
            format!("{TEST_REGISTRY}/navigator-web:26.6.23")
        );
    }

    #[test]
    fn a_registry_is_a_host_and_a_namespace() {
        let (host, namespace) = TEST_REGISTRY
            .split_once('/')
            .expect("the registry names a host and a namespace");
        assert_eq!(host, "ghcr.io");
        assert_eq!(namespace, "example/navigator");
        assert!(!is_artifact_registry(host));
    }

    #[test]
    fn registry_configuration_is_required() {
        for blank in [None, Some(String::new()), Some("   ".to_string())] {
            assert!(registry_from_value(blank).is_err());
        }
        assert_eq!(
            registry_from_value(Some(TEST_REGISTRY.into())).unwrap(),
            TEST_REGISTRY
        );
        assert_eq!(super::super::ship::images_registry(None), "");
    }

    #[test]
    fn a_malformed_registry_lookup_remains_an_error() {
        let error = tag_exists("not-a-registry", "navigator-web", "26.6.23")
            .expect_err("a malformed registry must not look like an absent tag");
        assert!(
            error.to_string().contains("is not `<host>/<namespace>`"),
            "{error}"
        );
        let digest_error = tag_digest("not-a-registry", "navigator-web", "26.6.23")
            .expect_err("a malformed registry must not look like an absent digest");
        assert!(digest_error.to_string().contains("cannot resolve digest"));
    }

    /// A fork overrides the one variable and every image follows.
    #[test]
    fn a_fork_overrides_the_whole_namespace() {
        assert_eq!(
            super::super::ship::images_registry(Some("ghcr.io/acme")),
            "ghcr.io/acme"
        );
    }
}

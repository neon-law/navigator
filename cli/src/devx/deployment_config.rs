//! Versioned installation coordinates in Secret Manager. The record contains
//! secret key names, never SOPS documents or credential values.
//!
//! The published record is the only source of a row. It is created once —
//! `ops deployments init` from flags, or `ops deployments import` from a
//! private `deployments/<row>/` tree — and edited in place by `ops deployments
//! set` / `unset` and `ops secrets set` / `unset`, each of which adds a new
//! Secret Manager version. The record stores only what cannot be derived;
//! readers fill in the rest through [`super::deployment_derivation`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

use super::deployment_derivation::{self as derivation, ANCHORS, LOCATION, PROJECT_ID};
use super::deployments::{is_env_var_name, Deployment};
use super::gcp::{auth, client::GcpClient, secret_manager, SetupError};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    deployment: Deployment,
}

/// Which secret key inventory a key name belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeySet {
    /// `web`'s env Secret (`encrypted_keys`).
    Web,
    /// The PostgreSQL service's file Secrets (`postgres_encrypted_keys`).
    Postgres,
    /// The Forgejo service's file Secrets (`forgejo_encrypted_keys`).
    Forgejo,
}

impl KeySet {
    fn of(self, deployment: &mut Deployment) -> &mut BTreeSet<String> {
        match self {
            Self::Web => &mut deployment.encrypted_keys,
            Self::Postgres => &mut deployment.postgres_encrypted_keys,
            Self::Forgejo => &mut deployment.forgejo_encrypted_keys,
        }
    }
}

/// One edit to a stored record. Applied all-or-nothing.
#[derive(Debug, Default)]
pub(super) struct Edit {
    /// Coordinates to store, overriding derived values.
    pub set: BTreeMap<String, String>,
    /// Stored coordinates to drop, falling back to the derived value if any.
    pub unset: BTreeSet<String>,
    /// Secret key names to add to an inventory.
    pub add_keys: Vec<(KeySet, String)>,
    /// Secret key names to remove from an inventory.
    pub remove_keys: Vec<(KeySet, String)>,
}

impl Edit {
    fn is_empty(&self) -> bool {
        self.set.is_empty()
            && self.unset.is_empty()
            && self.add_keys.is_empty()
            && self.remove_keys.is_empty()
    }
}

/// The inputs of `ops deployments init`.
#[derive(Debug)]
pub(super) struct Init {
    pub name: String,
    pub project: String,
    pub location: String,
    pub github_automation_home: bool,
    pub sms_test_recipients: bool,
    pub set: BTreeMap<String, String>,
}

pub(super) fn secret_id(name: &str) -> Result<String> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
        "deployment must contain only letters, numbers, hyphens, and underscores (1–128 characters)"
    );
    Ok(format!("navigator-deployment-config-{name}"))
}

fn validate_project(project: &str) -> Result<()> {
    ensure!(
        (6..=30).contains(&project.len())
            && project.starts_with(|c: char| c.is_ascii_lowercase())
            && project.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && project
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "--project-id must be a GCP project ID"
    );
    Ok(())
}

fn validate(deployment: &Deployment, project: &str, name: &str) -> Result<()> {
    ensure!(
        deployment.name == name,
        "cloud configuration names a different deployment"
    );
    ensure!(
        deployment.coordinates.get(PROJECT_ID).map(String::as_str) == Some(project),
        "cloud configuration belongs to a different GCP project"
    );
    ensure!(deployment.provisioned, "deployment is not provisioned");
    for key in deployment
        .encrypted_keys
        .iter()
        .chain(&deployment.postgres_encrypted_keys)
        .chain(&deployment.forgejo_encrypted_keys)
    {
        // Key names are not secret; naming them is what makes the error actionable.
        ensure!(
            is_env_var_name(key),
            "configuration contains an invalid secret key name `{key}`"
        );
        ensure!(
            !deployment.coordinates.contains_key(key),
            "`{key}` is both a secret key name and a plaintext coordinate in deployment {name}; \
             keep it in one place — a coordinate when `ops ship` renders it, a secret otherwise"
        );
    }
    Ok(())
}

/// Decode and check a stored record. Returns the record as stored, before
/// derivation, so an edit writes back only what was explicit.
fn decode_stored(bytes: &[u8], project: &str, name: &str) -> Result<Deployment> {
    // A serde error can contain input values. Do not attach it to the error chain.
    let record: Record = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("invalid deployment configuration record"))?;
    ensure!(
        record.version == 1,
        "unsupported deployment configuration version"
    );
    validate(&record.deployment, project, name)?;
    Ok(record.deployment)
}

/// Decode a record into the row readers use: stored coordinates over derived.
#[cfg(test)]
fn decode(bytes: &[u8], project: &str, name: &str) -> Result<Deployment> {
    let effective = derivation::effective(&decode_stored(bytes, project, name)?);
    validate(&effective, project, name)?;
    Ok(effective)
}

const NOT_FOUND: &str = "deployment configuration not found; create it with \
     `ops deployments init` or `ops deployments import` first";

async fn read_stored(client: &GcpClient, project: &str, name: &str) -> Result<Option<Deployment>> {
    validate_project(project)?;
    let id = secret_id(name)?;
    secret_manager::access_version(client, project, &id, "latest")
        .await
        .map_err(|error| explain_denied(error, project, name))?
        .map(|bytes| decode_stored(&bytes, project, name))
        .transpose()
}

async fn read_required(client: &GcpClient, project: &str, name: &str) -> Result<Deployment> {
    read_stored(client, project, name).await?.context(NOT_FOUND)
}

async fn load_with(client: &GcpClient, project: &str, name: &str) -> Result<Deployment> {
    let stored = read_required(client, project, name).await?;
    let effective = derivation::effective(&stored);
    validate(&effective, project, name)?;
    Ok(effective)
}

/// Secret Manager answers 403, not 404, for a secret the caller cannot read,
/// so a never-published row and a missing grant look the same. Name both.
fn explain_denied(error: SetupError, project: &str, name: &str) -> anyhow::Error {
    if !matches!(error, SetupError::BadStatus { status: 403, .. }) {
        return error.into();
    }
    anyhow::Error::from(error).context(format!(
        "deployment configuration for `{name}` is unreadable (403): either it was never \
         published, or this identity lacks roles/secretmanager.secretAccessor on it. Fix with:\n  \
         navigator ops deployments import --deployments-dir <private-root> {name}\n  \
         navigator ops gcp setup --project-id {project} --ship-deployment {name} \
         --ship-repository <owner/repo> --ship-environment <github-environment>"
    ))
}

/// Check a stored row the way every reader will see it, then add it as the
/// record's newest version. The one write path for every verb.
async fn write_with(client: &GcpClient, stored: Deployment) -> Result<()> {
    let id = secret_id(&stored.name)?;
    let project = stored
        .coordinates
        .get(PROJECT_ID)
        .cloned()
        .context("deployment configuration must set NAVIGATOR_GCP_PROJECT_ID")?;
    validate_project(&project)?;
    validate(&stored, &project, &stored.name)?;
    let effective = derivation::effective(&stored);
    validate(&effective, &project, &stored.name)?;
    super::ship::ShipConfig::from_deployment(&effective)?;
    let bytes = serde_json::to_vec(&Record {
        version: 1,
        deployment: stored,
    })?;
    secret_manager::ensure_secret(client, &project, &id).await?;
    secret_manager::add_version(client, &project, &id, &bytes).await?;
    Ok(())
}

/// Import a row read from a private tree. Refuses a row the ordered release
/// would refuse — the service key files are the gap that broke a release —
/// so the record never carries a row that cannot roll.
async fn import_with(client: &GcpClient, deployment: Deployment) -> Result<()> {
    super::ship::require_ordered_service_prerequisites(&derivation::effective(&deployment))?;
    write_with(client, deployment).await
}

fn single_line_value(key: &str, value: &str) -> Result<()> {
    ensure!(is_env_var_name(key), "`{key}` is not a coordinate name");
    ensure!(
        !value.trim().is_empty() && !value.contains(['\n', '\r']),
        "{key} must be a non-empty single line"
    );
    Ok(())
}

fn initial_row(init: Init) -> Result<Deployment> {
    use store::deployment::{GITHUB_AUTOMATION_HOME_FLAG, SMS_TEST_RECIPIENTS_FLAG};
    secret_id(&init.name)?;
    validate_project(&init.project)?;
    ensure!(
        !init.location.trim().is_empty()
            && init
                .location
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "--location must be a GCP location"
    );
    let mut coordinates = BTreeMap::from([
        (PROJECT_ID.to_owned(), init.project.clone()),
        (LOCATION.to_owned(), init.location.clone()),
        (
            GITHUB_AUTOMATION_HOME_FLAG.to_owned(),
            init.github_automation_home.to_string(),
        ),
        (
            SMS_TEST_RECIPIENTS_FLAG.to_owned(),
            init.sms_test_recipients.to_string(),
        ),
    ]);
    for (key, value) in init.set {
        ensure!(
            !coordinates.contains_key(&key),
            "{key} is set by its own flag, not --set"
        );
        single_line_value(&key, &value)?;
        coordinates.insert(key, value);
    }
    Ok(Deployment {
        name: init.name,
        kms_key: derivation::kms_key(&init.project, &init.location),
        provisioned: true,
        coordinates,
        encrypted_keys: BTreeSet::new(),
        postgres_encrypted_keys: BTreeSet::new(),
        forgejo_encrypted_keys: BTreeSet::new(),
    })
}

async fn init_with(client: &GcpClient, init: Init) -> Result<()> {
    let row = initial_row(init)?;
    let project = row.project_id().to_owned();
    if read_stored(client, &project, &row.name).await?.is_some() {
        bail!(
            "deployment configuration for {} already exists in {project}; edit it with \
             `ops deployments set` or `unset`",
            row.name
        );
    }
    write_with(client, row).await
}

/// Apply an edit to a stored row. Pure, so every refusal is unit-tested.
fn apply_edit(mut row: Deployment, edit: Edit) -> Result<Deployment> {
    ensure!(!edit.is_empty(), "nothing to change");
    for key in &edit.unset {
        ensure!(
            !ANCHORS.contains(&key.as_str()),
            "{key} anchors every derived coordinate and cannot be unset"
        );
        ensure!(
            row.coordinates.remove(key).is_some(),
            "{key} is not stored in deployment {}; nothing to unset",
            row.name
        );
    }
    for (key, value) in edit.set {
        single_line_value(&key, &value)?;
        ensure!(
            key != PROJECT_ID || row.coordinates.get(PROJECT_ID) == Some(&value),
            "a deployment cannot move to another GCP project; init a new row there"
        );
        row.coordinates.insert(key, value);
    }
    for (set, key) in edit.remove_keys {
        ensure!(
            set.of(&mut row).remove(&key),
            "secret key name {key} is not in deployment {}",
            row.name
        );
    }
    for (set, key) in edit.add_keys {
        ensure!(is_env_var_name(&key), "`{key}` is not a secret key name");
        set.of(&mut row).insert(key);
    }
    Ok(row)
}

async fn edit_with(client: &GcpClient, project: &str, name: &str, edit: Edit) -> Result<()> {
    let row = read_required(client, project, name).await?;
    write_with(client, apply_edit(row, edit)?).await
}

/// Write one secret value to Secret Manager, then record its key name. The
/// value is written first so the record never lists a key without a value.
async fn secret_set_with(
    client: &GcpClient,
    project: &str,
    name: &str,
    set: KeySet,
    key: &str,
    value: &[u8],
) -> Result<()> {
    ensure!(is_env_var_name(key), "`{key}` is not a secret key name");
    ensure!(
        !value.is_empty(),
        "refusing to store an empty value for {key}"
    );
    let mut row = read_required(client, project, name).await?;
    ensure!(
        !row.coordinates.contains_key(key),
        "{key} is a plaintext coordinate in deployment {name}; `ops deployments unset` it first"
    );
    secret_manager::ensure_secret(client, project, key).await?;
    secret_manager::add_version(client, project, key, value).await?;
    if set.of(&mut row).insert(key.to_owned()) {
        write_with(client, row).await?;
    }
    Ok(())
}

/// Remove a key name from whichever inventory holds it. Secret Manager keeps
/// the stored versions; nothing projects them once the name is gone.
async fn secret_unset_with(client: &GcpClient, project: &str, name: &str, key: &str) -> Result<()> {
    let mut row = read_required(client, project, name).await?;
    // Every inventory, not the first match: no stale copy may survive.
    let mut removed = false;
    for set in [KeySet::Web, KeySet::Postgres, KeySet::Forgejo] {
        removed |= set.of(&mut row).remove(key);
    }
    ensure!(removed, "secret key name {key} is not in deployment {name}");
    write_with(client, row).await
}

fn block_on<T>(work: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(work)
}

async fn adc_client() -> Result<GcpClient> {
    Ok(GcpClient::new(auth::adc_token_provider().await?))
}

/// Read configuration from an explicitly selected project without a checkout.
pub(super) fn load(project: &str, name: &str) -> Result<Deployment> {
    validate_project(project)?;
    secret_id(name)?;
    block_on(async { load_with(&adc_client().await?, project, name).await })
}

/// `ops deployments import` (and its alias `--publish`): publish coordinates
/// and inventories from an operator's local row. Does not decrypt or apply
/// credentials, provision infrastructure, or start a rollout.
pub(super) fn import(root: &Path, name: &str) -> Result<()> {
    secret_id(name)?;
    let deployment = Deployment::load(root, name)?;
    block_on(async { import_with(&adc_client().await?, deployment).await })?;
    eprintln!("deployment configuration imported; credential values unchanged");
    Ok(())
}

/// `ops deployments init`.
pub(super) fn init(init: Init) -> Result<()> {
    let name = init.name.clone();
    block_on(async { init_with(&adc_client().await?, init).await })?;
    eprintln!("deployment configuration {name} created");
    Ok(())
}

/// `ops deployments set` / `unset`.
pub(super) fn edit(project: &str, name: &str, edit: Edit) -> Result<()> {
    block_on(async { edit_with(&adc_client().await?, project, name, edit).await })?;
    eprintln!("deployment configuration {name} updated; credential values unchanged");
    Ok(())
}

/// `ops secrets set`: the value arrives on stdin and is never printed.
pub(super) fn secret_set(
    project: &str,
    name: &str,
    set: KeySet,
    key: &str,
    value: &[u8],
) -> Result<()> {
    block_on(async {
        secret_set_with(&adc_client().await?, project, name, set, key, value).await
    })?;
    eprintln!("wrote a new version of {key} in {project}");
    Ok(())
}

/// `ops secrets unset`.
pub(super) fn secret_unset(project: &str, name: &str, key: &str) -> Result<()> {
    block_on(async { secret_unset_with(&adc_client().await?, project, name, key).await })?;
    eprintln!("{key} removed from deployment {name}; its Secret Manager versions are retained");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, Request, ResponseTemplate,
    };

    use super::*;
    use crate::devx::gcp::client::{GcpService, StaticToken};

    fn fixture_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deployment-tree")
    }

    fn fixture() -> Record {
        Record {
            version: 1,
            deployment: Deployment::load(&fixture_root(), "example-deployment").unwrap(),
        }
    }

    fn client(server: &MockServer) -> GcpClient {
        GcpClient::new(Arc::new(StaticToken("synthetic".into())))
            .with_base_url(GcpService::SecretManager, server.uri())
    }

    fn encoded(record: &Record) -> serde_json::Value {
        serde_json::json!({
            "payload": {"data": STANDARD.encode(serde_json::to_vec(record).unwrap())}
        })
    }

    /// Serve `stored` (or a 404) as the record's latest version and accept
    /// every create and addVersion.
    async fn record_server(name: &str, stored: Option<&Record>) -> MockServer {
        let server = MockServer::start().await;
        let id = secret_id(name).unwrap();
        let latest = Mock::given(method("GET")).and(path(format!(
            "/v1/projects/example-project/secrets/{id}/versions/latest:access"
        )));
        match stored {
            Some(record) => {
                latest.respond_with(ResponseTemplate::new(200).set_body_json(encoded(record)))
            }
            None => latest.respond_with(ResponseTemplate::new(404)),
        }
        .mount(&server)
        .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        server
    }

    fn added_versions(requests: &[Request]) -> Vec<(String, Vec<u8>)> {
        requests
            .iter()
            .filter(|request| request.url.path().ends_with(":addVersion"))
            .map(|request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let bytes = STANDARD
                    .decode(body["payload"]["data"].as_str().unwrap())
                    .unwrap();
                (request.url.path().to_owned(), bytes)
            })
            .collect()
    }

    fn written_record(requests: &[Request], name: &str) -> Deployment {
        let id = secret_id(name).unwrap();
        let (_, bytes) = added_versions(requests)
            .into_iter()
            .find(|(path, _)| path.contains(&id))
            .expect("a record version was added");
        decode_stored(&bytes, "example-project", name).unwrap()
    }

    fn synthetic_init(name: &str) -> Init {
        Init {
            name: name.into(),
            project: "example-project".into(),
            location: "example-region".into(),
            github_automation_home: false,
            sms_test_recipients: false,
            set: [
                ("NAVIGATOR_ENVIRONMENT", "production"),
                ("NAVIGATOR_IMAGE_REGISTRY", "ghcr.io/example/navigator"),
                ("NAVIGATOR_WEB_IMAGE", "neon-server"),
                ("NAVIGATOR_PUBLIC_HOST", "navigator.example.invalid"),
                ("NAVIGATOR_PRIMARY_DOMAIN", "example.invalid"),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
        }
    }

    #[tokio::test]
    async fn cloud_load_fetches_only_the_selected_configuration() {
        let record = fixture();
        let project = record.deployment.project_id();
        let server = MockServer::start().await;
        let id = secret_id(&record.deployment.name).unwrap();
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/projects/{project}/secrets/{id}/versions/latest:access"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(encoded(&record)))
            .expect(1)
            .mount(&server)
            .await;
        let loaded = load_with(&client(&server), project, &record.deployment.name)
            .await
            .unwrap();
        assert_eq!(loaded, record.deployment);
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        let empty_root = tempfile::tempdir().unwrap();
        let options = super::super::ship::ShipOpts {
            deployment: loaded.name.clone(),
            project_id: Some(project.to_owned()),
            dry_run: true,
            image_only: true,
            tag: Some("26.10.21-rc.1".into()),
            ..Default::default()
        };
        super::super::ship::run_selected_ship(empty_root.path(), &loaded, &options).unwrap();
        assert!(!empty_root.path().join("deployments").exists());
    }

    #[tokio::test]
    async fn missing_or_denied_cloud_configuration_fails_without_fallback() {
        for status in [404, 403] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
            let result = load_with(&client(&server), "example-project", "example-deployment").await;
            assert!(result.is_err());
            let message = result.unwrap_err().to_string();
            if status == 404 {
                assert!(message.contains("deployments init"), "{message}");
            } else {
                assert!(message.contains("secretAccessor"), "{message}");
                assert!(message.contains("ops gcp setup"), "{message}");
                assert!(message.contains("deployments import"), "{message}");
            }
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn publishing_transfers_coordinates_and_names_without_sops_payloads() {
        let deployment = fixture().deployment;
        let project = deployment.project_id().to_owned();
        let name = deployment.name.clone();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(2)
            .mount(&server)
            .await;
        write_with(&client(&server), deployment.clone())
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let (_, bytes) = added_versions(&requests).pop().unwrap();
        assert_eq!(decode(&bytes, &project, &name).unwrap(), deployment);
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("ENC["));
        assert!(!text.contains("gcp_kms"));
        assert_eq!(requests.len(), 2);
    }

    /// A copy of the synthetic tree, with or without the two service files
    /// the ordered release requires.
    fn tree(with_service_files: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let row = dir.path().join("deployments/example-deployment");
        std::fs::create_dir_all(&row).unwrap();
        let source = fixture_root().join("deployments/example-deployment");
        for file in ["config.toml", "secrets.enc.yaml"] {
            std::fs::copy(source.join(file), row.join(file)).unwrap();
        }
        if with_service_files {
            let sops = |keys: &[&str]| {
                let mut body = keys
                    .iter()
                    .map(|key| format!("{key}: ENC[FIXTURE]\n"))
                    .collect::<Vec<_>>()
                    .concat();
                body.push_str("sops:\n    version: 3.9.0\n");
                body
            };
            std::fs::write(
                row.join("postgres-secrets.enc.yaml"),
                sops(&[
                    "NAVIGATOR_POSTGRES_PASSWORD",
                    "NAVIGATOR_POSTGRES_TLS_CERTIFICATE",
                    "NAVIGATOR_POSTGRES_TLS_PRIVATE_KEY",
                    "NAVIGATOR_POSTGRES_CA_CERT",
                    "NAVIGATOR_POSTGRES_APP_PASSWORD",
                    "NAVIGATOR_POSTGRES_MIGRATOR_PASSWORD",
                    "NAVIGATOR_POSTGRES_BACKUP_PASSWORD",
                ]),
            )
            .unwrap();
            std::fs::write(
                row.join("forgejo-secrets.enc.yaml"),
                sops(&[
                    "NAVIGATOR_FORGEJO_DATABASE_PASSWORD",
                    "NAVIGATOR_FORGEJO_SECRET_KEY",
                    "NAVIGATOR_FORGEJO_INTERNAL_TOKEN",
                    "NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET",
                    "NAVIGATOR_FORGEJO_POSTGRES_CA_CERT",
                    "NAVIGATOR_FORGEJO_OIDC_CLIENT_SECRET",
                ]),
            )
            .unwrap();
        }
        dir
    }

    /// The 26.10.22 gap: a row without its service key files must not reach
    /// the record, because the ordered release refuses it at roll time.
    #[tokio::test]
    async fn import_refuses_a_row_missing_its_service_key_files() {
        let server = MockServer::start().await;
        let root = tree(false);
        let deployment = Deployment::load(root.path(), "example-deployment").unwrap();
        let error = import_with(&client(&server), deployment)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("postgres-secrets.enc.yaml"), "{error}");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn import_publishes_a_complete_row_with_its_service_key_names() {
        let server = record_server("example-deployment", None).await;
        let root = tree(true);
        let deployment = Deployment::load(root.path(), "example-deployment").unwrap();
        import_with(&client(&server), deployment.clone())
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let written = written_record(&requests, "example-deployment");
        assert_eq!(written, deployment);
        assert!(written
            .forgejo_encrypted_keys
            .contains("NAVIGATOR_FORGEJO_OIDC_CLIENT_SECRET"));
    }

    #[tokio::test]
    async fn init_stores_only_what_cannot_be_derived() {
        let server = record_server("example-row", None).await;
        init_with(&client(&server), synthetic_init("example-row"))
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let written = written_record(&requests, "example-row");
        assert!(!written.coordinates.contains_key("NAVIGATOR_LOGS_BUCKET"));
        assert_eq!(
            written.coordinates[store::deployment::GITHUB_AUTOMATION_HOME_FLAG],
            "false"
        );
        assert_eq!(
            written.kms_key,
            crate::devx::gcp::kms::key_name("example-project", "example-region")
        );
        let effective = derivation::effective(&written);
        assert_eq!(
            effective.coordinates["NAVIGATOR_LOGS_BUCKET"],
            "example-project-logs"
        );
    }

    #[tokio::test]
    async fn init_refuses_an_existing_record() {
        let existing = Record {
            version: 1,
            deployment: initial_row(synthetic_init("example-row")).unwrap(),
        };
        let server = record_server("example-row", Some(&existing)).await;
        let error = init_with(&client(&server), synthetic_init("example-row"))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("already exists"), "{error}");
        assert!(added_versions(&server.received_requests().await.unwrap()).is_empty());
    }

    #[tokio::test]
    async fn init_refuses_a_row_ship_could_not_resolve() {
        let server = record_server("example-row", None).await;
        let mut init = synthetic_init("example-row");
        init.set.remove("NAVIGATOR_IMAGE_REGISTRY");
        let error = format!("{:#}", init_with(&client(&server), init).await.unwrap_err());
        assert!(error.contains("NAVIGATOR_IMAGE_REGISTRY"), "{error}");
        assert!(added_versions(&server.received_requests().await.unwrap()).is_empty());
    }

    #[tokio::test]
    async fn set_and_unset_edit_the_latest_record_and_add_a_version() {
        let stored = Record {
            version: 1,
            deployment: initial_row(synthetic_init("example-row")).unwrap(),
        };
        let server = record_server("example-row", Some(&stored)).await;
        let edit = Edit {
            set: BTreeMap::from([("NAVIGATOR_LOGS_BUCKET".into(), "example-logs".into())]),
            add_keys: vec![
                (KeySet::Web, "HCAPTCHA_SECRET".into()),
                (KeySet::Postgres, "NAVIGATOR_POSTGRES_PASSWORD".into()),
            ],
            ..Edit::default()
        };
        edit_with(&client(&server), "example-project", "example-row", edit)
            .await
            .unwrap();
        let written = written_record(&server.received_requests().await.unwrap(), "example-row");
        assert_eq!(written.coordinates["NAVIGATOR_LOGS_BUCKET"], "example-logs");
        assert!(written.encrypted_keys.contains("HCAPTCHA_SECRET"));
        assert!(written
            .postgres_encrypted_keys
            .contains("NAVIGATOR_POSTGRES_PASSWORD"));

        let server = record_server(
            "example-row",
            Some(&Record {
                version: 1,
                deployment: written,
            }),
        )
        .await;
        let edit = Edit {
            unset: BTreeSet::from(["NAVIGATOR_LOGS_BUCKET".into()]),
            remove_keys: vec![(KeySet::Web, "HCAPTCHA_SECRET".into())],
            ..Edit::default()
        };
        edit_with(&client(&server), "example-project", "example-row", edit)
            .await
            .unwrap();
        let written = written_record(&server.received_requests().await.unwrap(), "example-row");
        assert!(!written.coordinates.contains_key("NAVIGATOR_LOGS_BUCKET"));
        assert!(written.encrypted_keys.is_empty());
    }

    #[tokio::test]
    async fn editing_a_missing_record_fails_without_writing() {
        let server = record_server("example-row", None).await;
        let edit = Edit {
            set: BTreeMap::from([("NAVIGATOR_LOGS_BUCKET".into(), "example-logs".into())]),
            ..Edit::default()
        };
        let error = edit_with(&client(&server), "example-project", "example-row", edit)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("deployments init"), "{error}");
        assert!(added_versions(&server.received_requests().await.unwrap()).is_empty());
    }

    #[test]
    fn edits_refuse_anchors_moves_absent_names_and_multiline_values() {
        let row = || initial_row(synthetic_init("example-row")).unwrap();
        assert!(apply_edit(row(), Edit::default()).is_err());
        for anchor in ANCHORS {
            let edit = Edit {
                unset: BTreeSet::from([(*anchor).to_owned()]),
                ..Edit::default()
            };
            assert!(apply_edit(row(), edit).is_err(), "{anchor}");
        }
        let moved = Edit {
            set: BTreeMap::from([(PROJECT_ID.into(), "other-project".into())]),
            ..Edit::default()
        };
        assert!(apply_edit(row(), moved).is_err());
        let absent = Edit {
            unset: BTreeSet::from(["NAVIGATOR_LOGS_BUCKET".into()]),
            ..Edit::default()
        };
        assert!(apply_edit(row(), absent).is_err());
        let multiline = Edit {
            set: BTreeMap::from([("NAVIGATOR_LOGS_BUCKET".into(), "a\nb".into())]),
            ..Edit::default()
        };
        assert!(apply_edit(row(), multiline).is_err());
        let lowercase = Edit {
            add_keys: vec![(KeySet::Web, "not_a_key".into())],
            ..Edit::default()
        };
        assert!(apply_edit(row(), lowercase).is_err());
        let missing_key = Edit {
            remove_keys: vec![(KeySet::Forgejo, "NAVIGATOR_FORGEJO_SECRET_KEY".into())],
            ..Edit::default()
        };
        assert!(apply_edit(row(), missing_key).is_err());
    }

    #[test]
    fn init_flags_cannot_be_overridden_with_set() {
        for key in [
            PROJECT_ID,
            LOCATION,
            store::deployment::GITHUB_AUTOMATION_HOME_FLAG,
            store::deployment::SMS_TEST_RECIPIENTS_FLAG,
        ] {
            let mut init = synthetic_init("example-row");
            init.set.insert(key.into(), "other".into());
            assert!(initial_row(init).is_err(), "{key}");
        }
    }

    #[tokio::test]
    async fn secret_set_writes_the_value_then_records_only_its_name() {
        let stored = Record {
            version: 1,
            deployment: initial_row(synthetic_init("example-row")).unwrap(),
        };
        let server = record_server("example-row", Some(&stored)).await;
        secret_set_with(
            &client(&server),
            "example-project",
            "example-row",
            KeySet::Web,
            "HCAPTCHA_SECRET",
            b"synthetic-value",
        )
        .await
        .unwrap();
        let requests = server.received_requests().await.unwrap();
        let versions = added_versions(&requests);
        assert_eq!(versions.len(), 2);
        assert!(versions[0]
            .0
            .ends_with("/secrets/HCAPTCHA_SECRET:addVersion"));
        assert_eq!(versions[0].1, b"synthetic-value");
        let record = String::from_utf8(versions[1].1.clone()).unwrap();
        assert!(!record.contains("synthetic-value"));
        let written = written_record(&requests, "example-row");
        assert!(written.encrypted_keys.contains("HCAPTCHA_SECRET"));
    }

    #[tokio::test]
    async fn secret_set_refuses_a_coordinate_name_and_an_empty_value() {
        let stored = Record {
            version: 1,
            deployment: initial_row(synthetic_init("example-row")).unwrap(),
        };
        let server = record_server("example-row", Some(&stored)).await;
        let client = client(&server);
        for (key, value) in [
            ("NAVIGATOR_PUBLIC_HOST", &b"synthetic-value"[..]),
            ("HCAPTCHA_SECRET", &b""[..]),
        ] {
            assert!(secret_set_with(
                &client,
                "example-project",
                "example-row",
                KeySet::Web,
                key,
                value
            )
            .await
            .is_err());
        }
        assert!(added_versions(&server.received_requests().await.unwrap()).is_empty());
    }

    #[tokio::test]
    async fn secret_unset_drops_the_name_and_keeps_secret_manager_versions() {
        let mut deployment = initial_row(synthetic_init("example-row")).unwrap();
        deployment
            .forgejo_encrypted_keys
            .insert("NAVIGATOR_FORGEJO_SECRET_KEY".into());
        let stored = Record {
            version: 1,
            deployment,
        };
        let server = record_server("example-row", Some(&stored)).await;
        secret_unset_with(
            &client(&server),
            "example-project",
            "example-row",
            "NAVIGATOR_FORGEJO_SECRET_KEY",
        )
        .await
        .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert!(requests
            .iter()
            .all(|request| request.method.as_str() != "DELETE"));
        let written = written_record(&requests, "example-row");
        assert!(written.forgejo_encrypted_keys.is_empty());
        assert!(secret_unset_with(
            &client(&server),
            "example-project",
            "example-row",
            "NOT_IN_THE_ROW",
        )
        .await
        .is_err());
    }

    #[test]
    fn rejects_wrong_project_name_version_and_payload_fields() {
        let mut record = fixture();
        let project = record.deployment.project_id().to_owned();
        let name = record.deployment.name.clone();
        let bytes = serde_json::to_vec(&record).unwrap();
        assert!(decode(&bytes, "other-project", &name).is_err());
        assert!(decode(&bytes, &project, "other-deployment").is_err());
        record.version = 2;
        assert!(decode(&serde_json::to_vec(&record).unwrap(), &project, &name).is_err());
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["deployment"]["credential_values"] = serde_json::json!({"TOKEN": "synthetic-value"});
        let error = decode(&serde_json::to_vec(&value).unwrap(), &project, &name).unwrap_err();
        assert!(!format!("{error:#}").contains("synthetic-value"));
        assert!(decode(&serde_json::to_vec(&value).unwrap(), &project, &name).is_err());
    }

    #[test]
    fn rejects_secret_values_in_coordinates_and_unprovisioned_records() {
        let mut record = fixture();
        let project = record.deployment.project_id().to_owned();
        let name = record.deployment.name.clone();
        record.deployment.encrypted_keys.insert("TEST_TOKEN".into());
        record
            .deployment
            .coordinates
            .insert("TEST_TOKEN".into(), "synthetic-value".into());
        let error = validate(&record.deployment, &project, &name)
            .unwrap_err()
            .to_string();
        assert!(error.contains("`TEST_TOKEN`"));
        assert!(!error.contains("synthetic-value"));
        record.deployment.coordinates.remove("TEST_TOKEN");
        record.deployment.provisioned = false;
        assert!(validate(&record.deployment, &project, &name).is_err());
    }

    #[test]
    fn rejects_ambiguous_resource_selectors() {
        for name in ["", "../staging", "staging/other", "staging?version=1"] {
            assert!(secret_id(name).is_err());
        }
        for project in [
            "",
            "../other-project",
            "project/other",
            "123456",
            "project-",
        ] {
            assert!(validate_project(project).is_err());
        }
    }
}

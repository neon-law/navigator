//! `ops gcp access`: converge one project's secret-access posture onto a
//! declared `access.toml`.
//!
//! Three things, and nothing else:
//!
//! 1. **Audit.** Data Access (`DATA_READ`) logs for each declared service, so
//!    every secret read and every key use names its principal.
//! 2. **Secret encryption.** A `global` Cloud KMS key that Secret Manager
//!    encrypts every secret with (CMEK), rotated on the declared period. The
//!    Secret Manager service agent is the key's only encrypter/decrypter, so
//!    disabling the key cuts every secret off at once.
//! 3. **Deployment Readers.** Read-only service accounts — an outside agent, a
//!    dashboard — each holding only allowlisted read-only project roles and
//!    `secretAccessor` on the secrets it names, one secret at a time.
//!
//! The file is the whole truth for a declared reader: a role or secret the
//! file stops naming is revoked on the next run. A reader removed from the
//! file entirely is left alone — disable its account by hand.
//!
//! The declaration belongs beside the deployment row in the private
//! deployments tree; it names live accounts and secrets, which this public
//! repository never carries.
//!
//! [`Mode::Check`] reads everything, writes nothing, and returns the drift.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{json, Value};

use super::artifact_registry::project_number;
use super::client::{GcpClient, GcpService};
use super::error::{SetupError, SetupResult};

/// Project roles a Deployment Reader may hold. Each is read-only, and none
/// reads secret payloads, Kubernetes Secrets, or Data Access audit logs — the
/// audit trail of a reader's own reads stays out of that reader's reach.
pub const READER_PROJECT_ROLES: &[&str] = &[
    "roles/logging.viewer",
    "roles/monitoring.viewer",
    "roles/container.viewer",
];
const SECRET_ACCESSOR: &str = "roles/secretmanager.secretAccessor";
const KEY_ROLE: &str = "roles/cloudkms.cryptoKeyEncrypterDecrypter";
/// Automatic replication only accepts a key in `global`.
const KEY_LOCATION: &str = "global";
const KEY_RING: &str = super::kms::KEY_RING;
pub const SECRET_KEY: &str = "secret-manager";

/// Whether [`reconcile`] writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Apply,
    Check,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessFile {
    #[serde(default)]
    pub audit: Audit,
    pub secret_encryption: Option<SecretEncryption>,
    #[serde(default, rename = "reader")]
    pub readers: Vec<Reader>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audit {
    #[serde(default)]
    pub data_read: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretEncryption {
    pub rotation_days: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reader {
    pub account_id: String,
    pub display_name: String,
    #[serde(default)]
    pub project_roles: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
}

impl AccessFile {
    /// Parse and validate. Refuses any project role outside
    /// [`READER_PROJECT_ROLES`] before a single request is made.
    pub fn parse(text: &str) -> SetupResult<Self> {
        let file: Self = toml::from_str(text)
            .map_err(|error| SetupError::InvalidAccessFile(error.to_string()))?;
        let invalid = |detail: String| Err(SetupError::InvalidAccessFile(detail));
        for service in &file.audit.data_read {
            if !service.ends_with(".googleapis.com") {
                return invalid(format!(
                    "audit service `{service}` is not a googleapis.com service"
                ));
            }
        }
        if let Some(encryption) = &file.secret_encryption {
            if !(1..=365).contains(&encryption.rotation_days) {
                return invalid("secret_encryption.rotation_days must be 1 to 365".into());
            }
        }
        for reader in &file.readers {
            let id = &reader.account_id;
            let well_formed = (6..=30).contains(&id.len())
                && id.starts_with(|c: char| c.is_ascii_lowercase())
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !well_formed {
                return invalid(format!(
                    "reader account_id `{id}` is not a service-account id"
                ));
            }
            for role in &reader.project_roles {
                if !READER_PROJECT_ROLES.contains(&role.as_str()) {
                    return invalid(format!(
                        "reader `{id}` may not hold `{role}`; allowed: {}",
                        READER_PROJECT_ROLES.join(", ")
                    ));
                }
            }
        }
        Ok(file)
    }
}

fn reader_member(project_id: &str, reader: &Reader) -> String {
    format!(
        "serviceAccount:{}@{project_id}.iam.gserviceaccount.com",
        reader.account_id
    )
}

/// The `global` key every secret is encrypted with.
#[must_use]
pub fn secret_key_name(project_id: &str) -> String {
    format!(
        "projects/{project_id}/locations/{KEY_LOCATION}/keyRings/{KEY_RING}/cryptoKeys/{SECRET_KEY}"
    )
}

/// Converge `project_id` onto `file`. Returns one line per change made
/// ([`Mode::Apply`]) or needed ([`Mode::Check`]); empty means converged.
pub async fn reconcile(
    client: &GcpClient,
    project_id: &str,
    file: &AccessFile,
    mode: Mode,
) -> SetupResult<Vec<String>> {
    let mut drift = Vec::new();

    // Read first: a declared secret that does not exist fails the run
    // before anything is written.
    let secrets = list_secrets(client, project_id).await?;
    let missing: Vec<&str> = file
        .readers
        .iter()
        .flat_map(|reader| &reader.secrets)
        .filter(|name| !secrets.iter().any(|secret| secret.id == **name))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Err(SetupError::InvalidAccessFile(format!(
            "declared secrets do not exist in {project_id}: {}",
            missing.join(", ")
        )));
    }

    let key = match &file.secret_encryption {
        Some(encryption) => {
            Some(ensure_secret_key(client, project_id, encryption, mode, &mut drift).await?)
        }
        None => None,
    };
    for reader in &file.readers {
        ensure_reader_account(client, project_id, reader, mode, &mut drift).await?;
    }
    reconcile_project_policy(client, project_id, file, mode, &mut drift).await?;

    for secret in &secrets {
        if let Some(key) = &key {
            reconcile_secret_key(client, project_id, secret, key, mode, &mut drift).await?;
        }
        if !file.readers.is_empty() {
            let resource = Resource::Secret(project_id, &secret.id);
            let mut policy = read_policy(client, &resource).await?;
            let mut changed = false;
            for reader in &file.readers {
                let want = reader.secrets.contains(&secret.id);
                if set_member(
                    &mut policy,
                    SECRET_ACCESSOR,
                    &reader_member(project_id, reader),
                    want,
                ) {
                    changed = true;
                    drift.push(format!(
                        "{} {} read access to secret {}",
                        if want { "grant" } else { "revoke" },
                        reader.account_id,
                        secret.id
                    ));
                }
            }
            if changed && mode == Mode::Apply {
                write_policy(client, &resource, policy).await?;
            }
        }
    }
    Ok(drift)
}

struct Secret {
    id: String,
    json: Value,
}

async fn list_secrets(client: &GcpClient, project_id: &str) -> SetupResult<Vec<Secret>> {
    let mut secrets = Vec::new();
    let mut page = String::new();
    loop {
        let path = format!("/v1/projects/{project_id}/secrets?pageSize=250&pageToken={page}");
        let body = ok_json(client.get(GcpService::SecretManager, &path).await?, || {
            format!("list secrets in {project_id}")
        })?;
        for secret in body["secrets"].as_array().into_iter().flatten() {
            if let Some(id) = secret["name"]
                .as_str()
                .and_then(|name| name.rsplit('/').next())
            {
                secrets.push(Secret {
                    id: id.to_string(),
                    json: secret.clone(),
                });
            }
        }
        match body["nextPageToken"].as_str() {
            Some(token) if !token.is_empty() => page = token.to_string(),
            _ => return Ok(secrets),
        }
    }
}

async fn ensure_secret_key(
    client: &GcpClient,
    project_id: &str,
    encryption: &SecretEncryption,
    mode: Mode,
    drift: &mut Vec<String>,
) -> SetupResult<String> {
    let key = secret_key_name(project_id);
    let period = format!("{}s", u64::from(encryption.rotation_days) * 86_400);
    let next = (chrono::Utc::now() + chrono::Duration::days(i64::from(encryption.rotation_days)))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let existing = client
        .get(GcpService::CloudKms, &format!("/v1/{key}"))
        .await?;
    match existing.status_u16() {
        404 => {
            drift.push(format!("create key {key} rotating every {period}"));
            if mode == Mode::Check {
                drift.push(format!(
                    "make the Secret Manager service agent the only {KEY_ROLE} on {key}"
                ));
                return Ok(key);
            }
            let ring = client
                .post_json(
                    GcpService::CloudKms,
                    &format!(
                        "/v1/projects/{project_id}/locations/{KEY_LOCATION}/keyRings?keyRingId={KEY_RING}"
                    ),
                    &json!({}),
                )
                .await?;
            if !matches!(ring.status_u16(), 200..=299 | 409) {
                return Err(bad(
                    ring,
                    format!("create key ring {KEY_RING} in {KEY_LOCATION}"),
                ));
            }
            let created = client
                .post_json(
                    GcpService::CloudKms,
                    &format!(
                        "/v1/projects/{project_id}/locations/{KEY_LOCATION}/keyRings/{KEY_RING}\
                         /cryptoKeys?cryptoKeyId={SECRET_KEY}"
                    ),
                    &json!({"purpose": "ENCRYPT_DECRYPT", "rotationPeriod": period, "nextRotationTime": next}),
                )
                .await?;
            if !(200..=299).contains(&created.status_u16()) {
                return Err(bad(created, format!("create key {key}")));
            }
        }
        200..=299 => {
            let current = ok_json(existing, || format!("read key {key}"))?;
            if current["rotationPeriod"].as_str() != Some(period.as_str()) {
                drift.push(format!("set {key} rotation to {period}"));
                if mode == Mode::Apply {
                    let patched = client
                        .patch_json(
                            GcpService::CloudKms,
                            &format!("/v1/{key}?updateMask=rotationPeriod,nextRotationTime"),
                            &json!({"rotationPeriod": period, "nextRotationTime": next}),
                        )
                        .await?;
                    if !(200..=299).contains(&patched.status_u16()) {
                        return Err(bad(patched, format!("set rotation on {key}")));
                    }
                }
            }
        }
        _ => return Err(bad(existing, format!("read key {key}"))),
    }

    // The agent exists only once something asks for it; asking is idempotent.
    if mode == Mode::Apply {
        let identity = client
            .post_json(
                GcpService::ServiceUsage,
                &format!(
                    "/v1beta1/projects/{project_id}/services/secretmanager.googleapis.com\
                     :generateServiceIdentity"
                ),
                &json!({}),
            )
            .await?;
        if !(200..=299).contains(&identity.status_u16()) {
            return Err(bad(
                identity,
                "create the Secret Manager service agent".into(),
            ));
        }
    }
    let agent = format!(
        "serviceAccount:service-{}@gcp-sa-secretmanager.iam.gserviceaccount.com",
        project_number(client, project_id).await?
    );
    let resource = Resource::Key(&key);
    let mut policy = read_policy(client, &resource).await?;
    if set_sole_member(&mut policy, KEY_ROLE, &agent) {
        drift.push(format!("make {agent} the only {KEY_ROLE} on {key}"));
        if mode == Mode::Apply {
            write_policy(client, &resource, policy).await?;
        }
    }
    Ok(key)
}

async fn reconcile_secret_key(
    client: &GcpClient,
    project_id: &str,
    secret: &Secret,
    key: &str,
    mode: Mode,
    drift: &mut Vec<String>,
) -> SetupResult<()> {
    let automatic = &secret.json["replication"]["automatic"];
    if automatic.is_null() {
        drift.push(format!(
            "cannot encrypt secret {} with {SECRET_KEY}: it uses user-managed replication",
            secret.id
        ));
        return Ok(());
    }
    if automatic["customerManagedEncryption"]["kmsKeyName"].as_str() == Some(key) {
        return Ok(());
    }
    drift.push(format!("encrypt secret {} with {SECRET_KEY}", secret.id));
    if mode == Mode::Check {
        return Ok(());
    }
    let response = client
        .patch_json(
            GcpService::SecretManager,
            &format!("/v1/projects/{project_id}/secrets/{}?updateMask=replication", secret.id),
            &json!({"replication": {"automatic": {"customerManagedEncryption": {"kmsKeyName": key}}}}),
        )
        .await?;
    if (200..=299).contains(&response.status_u16()) {
        return Ok(());
    }
    Err(bad(
        response,
        format!("encrypt secret {} with {SECRET_KEY}", secret.id),
    ))
}

async fn ensure_reader_account(
    client: &GcpClient,
    project_id: &str,
    reader: &Reader,
    mode: Mode,
    drift: &mut Vec<String>,
) -> SetupResult<()> {
    let email = format!("{}@{project_id}.iam.gserviceaccount.com", reader.account_id);
    let existing = client
        .get(
            GcpService::Iam,
            &format!("/v1/projects/{project_id}/serviceAccounts/{email}"),
        )
        .await?;
    match existing.status_u16() {
        200..=299 => return Ok(()),
        404 => {}
        _ => return Err(bad(existing, format!("read service account {email}"))),
    }
    drift.push(format!("create service account {email}"));
    if mode == Mode::Check {
        return Ok(());
    }
    let created = client
        .post_json(
            GcpService::Iam,
            &format!("/v1/projects/{project_id}/serviceAccounts"),
            &json!({"accountId": reader.account_id, "serviceAccount": {"displayName": reader.display_name}}),
        )
        .await?;
    if matches!(created.status_u16(), 200..=299 | 409) {
        return Ok(());
    }
    Err(bad(created, format!("create service account {email}")))
}

/// Audit configs and reader project roles share one project-policy write.
async fn reconcile_project_policy(
    client: &GcpClient,
    project_id: &str,
    file: &AccessFile,
    mode: Mode,
    drift: &mut Vec<String>,
) -> SetupResult<()> {
    let resource = Resource::Project(project_id);
    let mut policy = read_policy(client, &resource).await?;
    let mut changed = false;
    for service in &file.audit.data_read {
        if ensure_data_read(&mut policy, service) {
            changed = true;
            drift.push(format!(
                "turn on Data Access (DATA_READ) logs for {service}"
            ));
        }
    }
    for reader in &file.readers {
        let member = reader_member(project_id, reader);
        let held: BTreeSet<String> = unconditional_roles(&policy, &member);
        for role in held
            .iter()
            .filter(|role| !reader.project_roles.contains(role))
        {
            set_member(&mut policy, role, &member, false);
            changed = true;
            drift.push(format!("revoke {role} from {}", reader.account_id));
        }
        for role in &reader.project_roles {
            if set_member(&mut policy, role, &member, true) {
                changed = true;
                drift.push(format!("grant {role} to {}", reader.account_id));
            }
        }
    }
    if changed && mode == Mode::Apply {
        write_policy(client, &resource, policy).await?;
    }
    Ok(())
}

enum Resource<'a> {
    Project(&'a str),
    Secret(&'a str, &'a str),
    Key(&'a str),
}

impl Resource<'_> {
    fn service_and_path(&self) -> (GcpService, String) {
        match self {
            Self::Project(project) => (
                GcpService::CloudResourceManager,
                format!("/v1/projects/{project}"),
            ),
            Self::Secret(project, secret) => (
                GcpService::SecretManager,
                format!("/v1/projects/{project}/secrets/{secret}"),
            ),
            Self::Key(key) => (GcpService::CloudKms, format!("/v1/{key}")),
        }
    }
}

/// Version-3 read, so conditional bindings survive the round trip.
async fn read_policy(client: &GcpClient, resource: &Resource<'_>) -> SetupResult<Value> {
    let (service, path) = resource.service_and_path();
    let response = match resource {
        // Resource Manager routes the read on POST; the others on GET.
        Resource::Project(_) => {
            client
                .post_json(
                    service,
                    &format!("{path}:getIamPolicy"),
                    &json!({"options": {"requestedPolicyVersion": 3}}),
                )
                .await?
        }
        _ => {
            client
                .get(
                    service,
                    &format!("{path}:getIamPolicy?options.requestedPolicyVersion=3"),
                )
                .await?
        }
    };
    ok_json(response, || format!("getIamPolicy for {path}"))
}

async fn write_policy(
    client: &GcpClient,
    resource: &Resource<'_>,
    mut policy: Value,
) -> SetupResult<()> {
    let (service, path) = resource.service_and_path();
    policy["version"] = json!(3);
    let mut body = json!({"policy": policy});
    if matches!(resource, Resource::Project(_)) {
        // Without the mask a project write ignores auditConfigs.
        body["updateMask"] = json!("auditConfigs,bindings,etag");
    }
    let response = client
        .post_json(service, &format!("{path}:setIamPolicy"), &body)
        .await?;
    if (200..=299).contains(&response.status_u16()) {
        return Ok(());
    }
    Err(bad(response, format!("setIamPolicy for {path}")))
}

fn unconditional(binding: &Value, role: &str) -> bool {
    binding["role"].as_str() == Some(role) && binding.get("condition").is_none()
}

fn unconditional_roles(policy: &Value, member: &str) -> BTreeSet<String> {
    policy["bindings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|binding| binding.get("condition").is_none())
        .filter(|binding| {
            binding["members"]
                .as_array()
                .is_some_and(|members| members.iter().any(|m| m.as_str() == Some(member)))
        })
        .filter_map(|binding| binding["role"].as_str().map(ToString::to_string))
        .collect()
}

/// Add or remove `member` on `role`'s unconditional binding. Conditional
/// bindings are never touched. Returns whether the policy changed.
fn set_member(policy: &mut Value, role: &str, member: &str, want: bool) -> bool {
    let present = unconditional_roles(policy, member).contains(role);
    if present == want {
        return false;
    }
    if policy.get("bindings").is_none() {
        policy["bindings"] = json!([]);
    }
    let Some(bindings) = policy["bindings"].as_array_mut() else {
        return false;
    };
    if want {
        match bindings
            .iter_mut()
            .find(|binding| unconditional(binding, role))
        {
            Some(binding) => {
                if let Some(members) = binding["members"].as_array_mut() {
                    members.push(json!(member));
                }
            }
            None => bindings.push(json!({"role": role, "members": [member]})),
        }
    } else {
        for binding in bindings
            .iter_mut()
            .filter(|binding| unconditional(binding, role))
        {
            if let Some(members) = binding["members"].as_array_mut() {
                members.retain(|m| m.as_str() != Some(member));
            }
        }
        bindings.retain(|binding| {
            !unconditional(binding, role)
                || binding["members"].as_array().is_some_and(|m| !m.is_empty())
        });
    }
    true
}

/// Make `member` the only unconditional holder of `role`.
fn set_sole_member(policy: &mut Value, role: &str, member: &str) -> bool {
    let others: Vec<String> = policy["bindings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|binding| unconditional(binding, role))
        .flat_map(|binding| binding["members"].as_array().into_iter().flatten())
        .filter_map(|m| m.as_str().filter(|m| *m != member).map(ToString::to_string))
        .collect();
    let mut changed = false;
    for other in others {
        changed |= set_member(policy, role, &other, false);
    }
    changed | set_member(policy, role, member, true)
}

/// Turn on `DATA_READ` for `service`, keeping its other log types and
/// exemptions. Returns whether the policy changed.
fn ensure_data_read(policy: &mut Value, service: &str) -> bool {
    if policy.get("auditConfigs").is_none() {
        policy["auditConfigs"] = json!([]);
    }
    let Some(configs) = policy["auditConfigs"].as_array_mut() else {
        return false;
    };
    let Some(config) = configs
        .iter_mut()
        .find(|config| config["service"].as_str() == Some(service))
    else {
        configs.push(json!({"service": service, "auditLogConfigs": [{"logType": "DATA_READ"}]}));
        return true;
    };
    if config.get("auditLogConfigs").is_none() {
        config["auditLogConfigs"] = json!([]);
    }
    let Some(logs) = config["auditLogConfigs"].as_array_mut() else {
        return false;
    };
    if logs
        .iter()
        .any(|log| log["logType"].as_str() == Some("DATA_READ"))
    {
        return false;
    }
    logs.push(json!({"logType": "DATA_READ"}));
    true
}

fn ok_json(
    response: super::client::HttpResponse,
    operation: impl FnOnce() -> String,
) -> SetupResult<Value> {
    if !(200..=299).contains(&response.status_u16()) {
        return Err(bad(response, operation()));
    }
    serde_json::from_str(&response.into_text()).map_err(|source| SetupError::Json {
        what: "GCP response",
        source,
    })
}

fn bad(response: super::client::HttpResponse, operation: String) -> SetupError {
    SetupError::BadStatus {
        operation,
        status: response.status_u16(),
        body: response.into_text(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::client::StaticToken;
    use super::*;

    const PROJECT: &str = "example-project";
    const READER: &str = "serviceAccount:example-reader@example-project.iam.gserviceaccount.com";

    fn file(extra: &str) -> AccessFile {
        AccessFile::parse(&format!(
            r#"
[audit]
data_read = ["secretmanager.googleapis.com"]

[[reader]]
account_id = "example-reader"
display_name = "Example reader"
project_roles = ["roles/logging.viewer"]
secrets = ["DECLARED"]
{extra}"#
        ))
        .expect("the fixture parses")
    }

    async fn server() -> (MockServer, GcpClient) {
        let server = MockServer::start().await;
        let mut client = GcpClient::new(Arc::new(StaticToken("test-token".into())));
        for service in [
            GcpService::SecretManager,
            GcpService::CloudResourceManager,
            GcpService::Iam,
            GcpService::CloudKms,
            GcpService::ServiceUsage,
        ] {
            client = client.with_base_url(service, server.uri());
        }
        (server, client)
    }

    async fn mount_world(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path(format!("/v1/projects/{PROJECT}/secrets")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"secrets": [
                {"name": "projects/1/secrets/DECLARED", "replication": {"automatic": {}}},
                {"name": "projects/1/secrets/UNDECLARED", "replication": {"automatic": {}}}
            ]})))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/projects/{PROJECT}/serviceAccounts/example-reader@{PROJECT}.iam.gserviceaccount.com"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(server)
            .await;
        // The reader holds an admin role by hand, and read access to the
        // secret the file does not name.
        Mock::given(method("POST"))
            .and(path(format!("/v1/projects/{PROJECT}:getIamPolicy")))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"etag": "e1", "bindings": [
                    {"role": "roles/owner", "members": ["user:owner@example.com"]},
                    {"role": "roles/secretmanager.admin", "members": [READER]}
                ]}),
            ))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/projects/{PROJECT}/secrets/DECLARED:getIamPolicy"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"etag": "d"})))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v1/projects/{PROJECT}/secrets/UNDECLARED:getIamPolicy")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"etag": "u", "bindings": [
                {"role": SECRET_ACCESSOR, "members": [READER, "serviceAccount:runtime@example.com"]}
            ]})))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn apply_converges_audit_roles_and_per_secret_access() {
        let (server, client) = server().await;
        mount_world(&server).await;
        Mock::given(method("POST"))
            .and(path(format!("/v1/projects/{PROJECT}:setIamPolicy")))
            .and(body_partial_json(json!({"updateMask": "auditConfigs,bindings,etag", "policy": {
                "etag": "e1",
                "auditConfigs": [{"service": "secretmanager.googleapis.com", "auditLogConfigs": [{"logType": "DATA_READ"}]}],
                "bindings": [
                    {"role": "roles/owner", "members": ["user:owner@example.com"]},
                    {"role": "roles/logging.viewer", "members": [READER]}
                ]
            }})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/v1/projects/{PROJECT}/secrets/DECLARED:setIamPolicy"
            )))
            .and(body_partial_json(
                json!({"policy": {"bindings": [{"role": SECRET_ACCESSOR, "members": [READER]}]}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/v1/projects/{PROJECT}/secrets/UNDECLARED:setIamPolicy"
            )))
            .and(body_partial_json(json!({"policy": {"bindings": [
                {"role": SECRET_ACCESSOR, "members": ["serviceAccount:runtime@example.com"]}
            ]}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let drift = reconcile(&client, PROJECT, &file(""), Mode::Apply)
            .await
            .unwrap();
        assert_eq!(
            drift,
            [
                "turn on Data Access (DATA_READ) logs for secretmanager.googleapis.com",
                "revoke roles/secretmanager.admin from example-reader",
                "grant roles/logging.viewer to example-reader",
                "grant example-reader read access to secret DECLARED",
                "revoke example-reader read access to secret UNDECLARED",
            ]
        );
    }

    #[tokio::test]
    async fn check_reports_the_same_drift_and_writes_nothing() {
        let (server, client) = server().await;
        mount_world(&server).await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::path_regex(":setIamPolicy$"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let drift = reconcile(&client, PROJECT, &file(""), Mode::Check)
            .await
            .unwrap();
        assert_eq!(drift.len(), 5, "{drift:?}");
    }

    #[tokio::test]
    async fn a_missing_declared_secret_fails_before_any_write() {
        let (server, client) = server().await;
        Mock::given(method("GET"))
            .and(path(format!("/v1/projects/{PROJECT}/secrets")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"secrets": []})))
            .expect(1)
            .mount(&server)
            .await;
        let error = reconcile(&client, PROJECT, &file(""), Mode::Apply)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("DECLARED"), "{error}");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn encryption_creates_a_rotating_global_key_owned_by_the_agent_and_points_secrets_at_it()
    {
        let (server, client) = server().await;
        mount_world(&server).await;
        let key = secret_key_name(PROJECT);
        Mock::given(method("GET"))
            .and(path(format!("/v1/{key}")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/v1/projects/{PROJECT}/locations/global/keyRings"
            )))
            .respond_with(ResponseTemplate::new(409))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/v1/projects/{PROJECT}/locations/global/keyRings/{KEY_RING}/cryptoKeys"
            )))
            .and(query_param("cryptoKeyId", SECRET_KEY))
            .and(body_partial_json(
                json!({"purpose": "ENCRYPT_DECRYPT", "rotationPeriod": "7776000s"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::path_regex(":generateServiceIdentity$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"done": true})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v3/projects/{PROJECT}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "projects/123"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v1/{key}:getIamPolicy")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"bindings": [
                    {"role": KEY_ROLE, "members": ["user:someone@example.com"]}
                ]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/v1/{key}:setIamPolicy")))
            .and(body_partial_json(
                json!({"policy": {"bindings": [{"role": KEY_ROLE, "members": [
                    "serviceAccount:service-123@gcp-sa-secretmanager.iam.gserviceaccount.com"
                ]}]}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        for secret in ["DECLARED", "UNDECLARED"] {
            Mock::given(method("PATCH"))
                .and(path(format!("/v1/projects/{PROJECT}/secrets/{secret}")))
                .and(query_param("updateMask", "replication"))
                .and(body_partial_json(json!({"replication": {"automatic": {
                    "customerManagedEncryption": {"kmsKeyName": key}
                }}})))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("POST"))
            .and(wiremock::matchers::path_regex(":setIamPolicy$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let drift = reconcile(
            &client,
            PROJECT,
            &file("[secret_encryption]\nrotation_days = 90\n"),
            Mode::Apply,
        )
        .await
        .unwrap();
        assert!(
            drift.contains(&"encrypt secret UNDECLARED with secret-manager".to_string()),
            "{drift:?}"
        );
    }

    #[test]
    fn a_reader_cannot_be_declared_with_a_role_outside_the_allowlist() {
        for role in [
            "roles/owner",
            "roles/secretmanager.secretAccessor",
            "roles/logging.privateLogViewer",
        ] {
            let text = format!(
                "[[reader]]\naccount_id = \"example-reader\"\ndisplay_name = \"x\"\nproject_roles = [\"{role}\"]\n"
            );
            let error = AccessFile::parse(&text).unwrap_err();
            assert!(error.to_string().contains(role), "{error}");
        }
    }

    #[test]
    fn set_member_leaves_conditional_bindings_alone() {
        let condition = json!({"title": "t", "expression": "true"});
        let mut policy = json!({"bindings": [{"role": SECRET_ACCESSOR, "members": [READER], "condition": condition}]});
        assert!(!set_member(&mut policy, SECRET_ACCESSOR, READER, false));
        assert!(set_member(&mut policy, SECRET_ACCESSOR, READER, true));
        assert_eq!(policy["bindings"].as_array().unwrap().len(), 2);
    }
}

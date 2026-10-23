//! The release-ship identity: what `ops ship` needs when GitHub Actions runs it.
//!
//! `navigator-release-ship` is created with its transcript grant next to the
//! logs bucket (see [`super::workload_identity::ensure_ship_transcript_writer`]).
//! This stage gives it the rest of what a full `ops ship` uses and lets one
//! GitHub Environment impersonate it keyless:
//!
//! 1. Project roles `container.clusterViewer`, `container.developer`, and
//!    `secretmanager.viewer`, plus the custom [`SIGNING_IAM_READER_ROLE_ID`]
//!    role (created when absent). The custom role is bound on the project
//!    because a deny policy blocks `setIamPolicy` on the web service account
//!    for everyone but its admin, so a resource-level binding cannot be written.
//! 2. `artifactregistry.reader` on the image repository — the hub's when
//!    `--images-project-id` is set — and `storage.objectViewer` on the assets
//!    bucket.
//! 3. `secretmanager.secretAccessor` on the one
//!    `navigator-deployment-config-<row>` secret. The secret shell is created
//!    when absent so the binding can be written; no version is added.
//! 4. The `navigator-release` Workload Identity pool, its `github` OIDC
//!    provider pinned to one repository, one environment, and `main`, and
//!    `iam.workloadIdentityUser` for that environment's principal set.
//!
//! The row, repository, and environment are explicit flags. Nothing here infers
//! them, because a wrong guess grants a deployment to the wrong workflow.
//!
//! Every IAM write reads the live policy first and skips the write when the
//! binding is already there, so a re-run changes nothing.

use serde_json::{json, Value};

use super::artifact_registry::{
    self, create_lro_or_conflict, ensure_iam_member, patch_lro, read_wif_provider,
    save_wif_provider_preimage, BindingOutcome, EnsureOutcome, MemberMode, PolicyRead,
    GITHUB_OIDC_ISSUER, READER_ROLE,
};
use super::client::{GcpClient, GcpService};
use super::error::{SetupError, SetupResult};
use super::workload_identity::{classify, RELEASE_SHIP_ACCOUNT_ID};
use super::{secret_manager, SetupConfig};

/// The ship identity's own Workload Identity pool and provider.
pub const RELEASE_WIF_POOL_ID: &str = "navigator-release";
pub const RELEASE_WIF_PROVIDER_ID: &str = "github";
/// Custom role for `ops ship --assert-signing-iam`.
pub const SIGNING_IAM_READER_ROLE_ID: &str = "navigatorSigningIamReader";
const SIGNING_IAM_READER_PERMISSION: &str = "iam.serviceAccounts.getIamPolicy";
/// Predefined project roles. `clusterViewer` covers `get-credentials`;
/// `developer` covers the apply and rollout reads; `secretmanager.viewer` reads
/// version metadata for the enabled-version check, never a payload.
pub const PROJECT_ROLES: &[&str] = &[
    "roles/container.clusterViewer",
    "roles/container.developer",
    "roles/secretmanager.viewer",
];
const ASSETS_ROLE: &str = "roles/storage.objectViewer";
const CONFIG_SECRET_ROLE: &str = "roles/secretmanager.secretAccessor";
/// The `ConfigMap` naming the PostgreSQL backups bucket for the backup
/// workloads. `ops ship` refuses an ordered release without it.
pub const POSTGRES_BACKUP_TARGET_CONFIGMAP: &str = "navigator-postgres-backup-target";
const FIELD_MANAGER: &str = "navigator-gcp-setup";

/// The three explicit inputs that bind one row's ship to one GitHub workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseShipConfig {
    /// The deployment row whose `navigator-deployment-config-<row>` the ship
    /// reads.
    pub deployment: String,
    /// `owner/repo` whose workflow runs the ship.
    pub repository: String,
    /// GitHub Environment the ship job runs in (e.g. `staging-deploy`).
    pub environment: String,
}

impl ReleaseShipConfig {
    /// Refuse a value that could widen the provider condition or the principal
    /// set. Runs before the first GCP call.
    pub fn validate(&self) -> SetupResult<()> {
        self.config_secret_id()?;
        let repository_ok = self
            .repository
            .split_once('/')
            .is_some_and(|(owner, name)| {
                artifact_registry::valid_github_owner(owner) && valid_github_name(name)
            });
        if !repository_ok {
            return Err(SetupError::InvalidFlag {
                flag: "--ship-repository",
                detail: "expected owner/repo",
            });
        }
        if self.environment.len() > 255 || !valid_github_name(&self.environment) {
            return Err(SetupError::InvalidFlag {
                flag: "--ship-environment",
                detail: "use only letters, numbers, `.`, `_`, and `-`",
            });
        }
        Ok(())
    }

    /// The configuration secret this ship reads — the same name
    /// `ops deployments --publish` writes.
    pub fn config_secret_id(&self) -> SetupResult<String> {
        super::super::deployment_config::secret_id(&self.deployment).map_err(|_| {
            SetupError::InvalidFlag {
                flag: "--ship-deployment",
                detail: "use only letters, numbers, hyphens, and underscores (1-128 characters)",
            }
        })
    }
}

fn valid_github_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// `navigator-release-ship@<project>.iam.gserviceaccount.com`.
#[must_use]
pub fn service_account_email(project_id: &str) -> String {
    format!("{RELEASE_SHIP_ACCOUNT_ID}@{project_id}.iam.gserviceaccount.com")
}

/// The token-exchange condition: one repository, one environment, `main`.
#[must_use]
pub fn attribute_condition(ship: &ReleaseShipConfig) -> String {
    format!(
        "assertion.repository == '{}' && assertion.environment == '{}' && \
         assertion.ref == 'refs/heads/main'",
        ship.repository, ship.environment
    )
}

/// The environment's principal set in the `navigator-release` pool.
#[must_use]
pub fn principal_set(project_number: &str, environment: &str) -> String {
    format!(
        "principalSet://iam.googleapis.com/projects/{project_number}/locations/global/\
         workloadIdentityPools/{RELEASE_WIF_POOL_ID}/attribute.environment/{environment}"
    )
}

fn provider_body(ship: &ReleaseShipConfig) -> Value {
    json!({
        "displayName": "GitHub release ship",
        "oidc": { "issuerUri": GITHUB_OIDC_ISSUER },
        "attributeMapping": {
            "google.subject": "assertion.sub",
            "attribute.repository": "assertion.repository",
            "attribute.environment": "assertion.environment"
        },
        "attributeCondition": attribute_condition(ship)
    })
}

/// Provision everything in the module docs, in order. The service account
/// itself already exists: stage 8 created it with the transcript grant.
pub async fn ensure(
    client: &GcpClient,
    project_id: &str,
    config: &SetupConfig,
    ship: &ReleaseShipConfig,
    assets_bucket: &str,
) -> SetupResult<()> {
    ship.validate()?;
    let member = format!("serviceAccount:{}", service_account_email(project_id));

    let custom_role = ensure_signing_iam_reader_role(client, project_id).await?;
    for role in PROJECT_ROLES.iter().copied().chain([custom_role.as_str()]) {
        ensure_project_member(client, project_id, role, &member).await?;
    }

    let images_project_id = config.images_project_id.as_deref().unwrap_or(project_id);
    artifact_registry::ensure_repo_iam_member(
        client,
        images_project_id,
        &config.region,
        &config.artifact_registry_repo,
        READER_ROLE,
        &member,
    )
    .await?;
    ensure_bucket_member(client, assets_bucket, ASSETS_ROLE, &member).await?;

    let secret_id = ship.config_secret_id()?;
    secret_manager::ensure_secret(client, project_id, &secret_id).await?;
    ensure_iam_member(
        client,
        GcpService::SecretManager,
        &format!("/v1/projects/{project_id}/secrets/{secret_id}"),
        CONFIG_SECRET_ROLE,
        &member,
        PolicyRead::Get,
        MemberMode::Additive,
    )
    .await?;

    artifact_registry::ensure_named_wif_pool(
        client,
        project_id,
        RELEASE_WIF_POOL_ID,
        "Navigator release ship",
    )
    .await?;
    ensure_provider(client, project_id, ship).await?;
    let project_number = artifact_registry::project_number(client, project_id).await?;
    artifact_registry::ensure_wif_impersonation(
        client,
        project_id,
        &service_account_email(project_id),
        &principal_set(&project_number, &ship.environment),
    )
    .await?;
    Ok(())
}

/// Add `member` to `role` on the project's policy, read at version 3 so a
/// conditional binding elsewhere in the policy survives the write.
pub async fn ensure_project_member(
    client: &GcpClient,
    project_id: &str,
    role: &str,
    member: &str,
) -> SetupResult<BindingOutcome> {
    ensure_iam_member(
        client,
        GcpService::CloudResourceManager,
        &format!("/v1/projects/{project_id}"),
        role,
        member,
        PolicyRead::PostV3,
        MemberMode::Additive,
    )
    .await
}

/// Create the custom role when absent; restore it when soft-deleted; add its
/// one permission when an existing role lacks it. Returns the role's full name.
pub async fn ensure_signing_iam_reader_role(
    client: &GcpClient,
    project_id: &str,
) -> SetupResult<String> {
    let collection = format!("/v1/projects/{project_id}/roles");
    let role_path = format!("{collection}/{SIGNING_IAM_READER_ROLE_ID}");
    let name = format!("projects/{project_id}/roles/{SIGNING_IAM_READER_ROLE_ID}");
    let body = json!({
        "roleId": SIGNING_IAM_READER_ROLE_ID,
        "role": {
            "title": "Navigator signing IAM reader",
            "description": "Read service-account IAM policy for ops ship --assert-signing-iam",
            "includedPermissions": [SIGNING_IAM_READER_PERMISSION],
            "stage": "GA"
        }
    });
    let response = client
        .post_json(GcpService::Iam, &collection, &body)
        .await?;
    match response.status_u16() {
        200..=299 => return Ok(name),
        409 => {}
        status => {
            return Err(SetupError::BadStatus {
                operation: format!("create custom role {SIGNING_IAM_READER_ROLE_ID}"),
                status,
                body: response.into_text(),
            })
        }
    }

    let live = read_json(client, GcpService::Iam, &role_path, "read custom role").await?;
    if live.get("deleted").and_then(Value::as_bool) == Some(true) {
        let etag = live.get("etag").cloned().unwrap_or(Value::Null);
        expect_ok(
            client
                .post_json(
                    GcpService::Iam,
                    &format!("{role_path}:undelete"),
                    &json!({ "etag": etag }),
                )
                .await?,
            "undelete custom role",
        )?;
    }
    let mut permissions: Vec<Value> = live
        .get("includedPermissions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !permissions
        .iter()
        .any(|p| p.as_str() == Some(SIGNING_IAM_READER_PERMISSION))
    {
        permissions.push(json!(SIGNING_IAM_READER_PERMISSION));
        expect_ok(
            client
                .patch_json(
                    GcpService::Iam,
                    &format!("{role_path}?updateMask=includedPermissions"),
                    &json!({ "includedPermissions": permissions }),
                )
                .await?,
            "add permission to custom role",
        )?;
    }
    Ok(name)
}

/// Add `member` to `role` on a bucket's policy. Cloud Storage reads the policy
/// with a `GET` on `/iam` and writes it with a `PUT`, not the `:setIamPolicy`
/// verb every other surface here uses.
pub async fn ensure_bucket_member(
    client: &GcpClient,
    bucket: &str,
    role: &str,
    member: &str,
) -> SetupResult<BindingOutcome> {
    let iam_path = format!("/storage/v1/b/{bucket}/iam");
    let mut policy = read_json(
        client,
        GcpService::Storage,
        &format!("{iam_path}?optionsRequestedPolicyVersion=3"),
        "read bucket IAM policy",
    )
    .await?;
    if artifact_registry::policy_contains_member(&policy, role, member) {
        return Ok(BindingOutcome::AlreadyPresent);
    }
    artifact_registry::upsert_member(&mut policy, role, member);
    policy["version"] = json!(3);
    expect_ok(
        client
            .put_json(GcpService::Storage, &iam_path, &policy)
            .await?,
        "write bucket IAM policy",
    )?;
    Ok(BindingOutcome::Added)
}

/// Create the provider, or converge an existing one onto this repository and
/// environment after saving its pre-image.
async fn ensure_provider(
    client: &GcpClient,
    project_id: &str,
    ship: &ReleaseShipConfig,
) -> SetupResult<EnsureOutcome> {
    let collection = format!(
        "/v1/projects/{project_id}/locations/global/workloadIdentityPools/\
         {RELEASE_WIF_POOL_ID}/providers"
    );
    let body = provider_body(ship);
    let outcome = create_lro_or_conflict(
        client,
        GcpService::Iam,
        &format!("{collection}?workloadIdentityPoolProviderId={RELEASE_WIF_PROVIDER_ID}"),
        &body,
        "create release WIF provider",
    )
    .await?;
    if outcome == EnsureOutcome::Created {
        return Ok(outcome);
    }

    let provider_path = format!("{collection}/{RELEASE_WIF_PROVIDER_ID}");
    let live = read_wif_provider(client, &provider_path, "read release WIF provider").await?;
    if live.get("state").and_then(Value::as_str) == Some("DELETED") {
        return Err(SetupError::AmbiguousLiveState {
            operation: "update release WIF provider".to_string(),
            detail: "the provider is soft-deleted; undelete it or wait for its purge, then re-run"
                .to_string(),
        });
    }
    if provider_matches(&live, &body) {
        return Ok(outcome);
    }
    let backup_path = save_wif_provider_preimage(&live)?;
    eprintln!(
        "release WIF provider pre-image saved to {}",
        backup_path.display()
    );
    patch_lro(
        client,
        GcpService::Iam,
        &format!("{provider_path}?updateMask=attributeCondition,attributeMapping,oidc"),
        &body,
        "update release WIF provider",
    )
    .await?;
    let after = read_wif_provider(client, &provider_path, "read release WIF provider").await?;
    if !provider_matches(&after, &body) {
        return Err(SetupError::AmbiguousLiveState {
            operation: "update release WIF provider".to_string(),
            detail: format!(
                "verification failed; pre-image is at {}",
                backup_path.display()
            ),
        });
    }
    Ok(outcome)
}

fn provider_matches(live: &Value, wanted: &Value) -> bool {
    live.get("attributeCondition") == wanted.get("attributeCondition")
        && live.get("attributeMapping") == wanted.get("attributeMapping")
        && live.pointer("/oidc/issuerUri") == wanted.pointer("/oidc/issuerUri")
}

/// Server-side apply the backup-target `ConfigMap` (and its Namespace, which
/// may not exist before the first ship) into the row's namespace, against the
/// context `get-credentials` writes for this cluster.
pub async fn ensure_postgres_backup_target(
    client: &GcpClient,
    project_id: &str,
    config: &SetupConfig,
    backups_bucket: &str,
) -> SetupResult<()> {
    classify(
        client
            .shell_out(
                "gcloud",
                &[
                    "container",
                    "clusters",
                    "get-credentials",
                    &config.cluster_name,
                    "--location",
                    &config.region,
                    "--project",
                    project_id,
                ],
            )
            .await?,
        "fetch cluster credentials",
    )?;
    let context = format!("gke_{project_id}_{}_{}", config.region, config.cluster_name);
    let manifest = postgres_backup_target_manifest(&config.kubernetes_namespace, backups_bucket);
    classify(
        client
            .shell_out_with_stdin(
                "kubectl",
                &[
                    "--context",
                    &context,
                    "apply",
                    "--server-side",
                    "--field-manager",
                    FIELD_MANAGER,
                    "--filename",
                    "-",
                ],
                Some(&manifest),
            )
            .await?,
        "apply PostgreSQL backup target ConfigMap",
    )
}

fn postgres_backup_target_manifest(namespace: &str, bucket: &str) -> String {
    format!(
        "apiVersion: v1
kind: Namespace
metadata:
  name: {namespace}
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: {POSTGRES_BACKUP_TARGET_CONFIGMAP}
  namespace: {namespace}
data:
  bucket: {bucket}
"
    )
}

async fn read_json(
    client: &GcpClient,
    service: GcpService,
    path: &str,
    operation: &'static str,
) -> SetupResult<Value> {
    let response = client.get(service, path).await?;
    let status = response.status_u16();
    let body = response.into_text();
    if !(200..=299).contains(&status) {
        return Err(SetupError::BadStatus {
            operation: operation.to_string(),
            status,
            body,
        });
    }
    serde_json::from_str(&body).map_err(|source| SetupError::Json {
        what: operation,
        source,
    })
}

fn expect_ok(response: super::client::HttpResponse, operation: &'static str) -> SetupResult<()> {
    let status = response.status_u16();
    if (200..=299).contains(&status) {
        return Ok(());
    }
    Err(SetupError::BadStatus {
        operation: operation.to_string(),
        status,
        body: response.into_text(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method, path, path_regex, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::client::StaticToken;
    use super::*;

    const PROJECT: &str = "example-staging";
    const MEMBER: &str =
        "serviceAccount:navigator-release-ship@example-staging.iam.gserviceaccount.com";
    const CUSTOM_ROLE: &str = "projects/example-staging/roles/navigatorSigningIamReader";
    const SA_POLICY_PATH: &str = "/v1/projects/example-staging/serviceAccounts/\
         navigator-release-ship@example-staging.iam.gserviceaccount.com:getIamPolicy";
    const PROVIDERS_PATH: &str = "/v1/projects/example-staging/locations/global/\
         workloadIdentityPools/navigator-release/providers";
    const PROVIDER_PATH: &str = "/v1/projects/example-staging/locations/global/\
         workloadIdentityPools/navigator-release/providers/github";
    const REPO_PATH: &str =
        "/v1/projects/example-staging/locations/example-region/repositories/navigator";
    const SECRET_PATH: &str =
        "/v1/projects/example-staging/secrets/navigator-deployment-config-example-a";

    fn ship() -> ReleaseShipConfig {
        ReleaseShipConfig {
            deployment: "example-a".into(),
            repository: "example-org/navigator".into(),
            environment: "staging-deploy".into(),
        }
    }

    fn config() -> SetupConfig {
        SetupConfig {
            region: "example-region".into(),
            release_ship: Some(ship()),
            ..SetupConfig::default()
        }
    }

    fn client_for(server: &MockServer) -> GcpClient {
        let mut client = GcpClient::new(Arc::new(StaticToken("t".into())));
        for service in [
            GcpService::Iam,
            GcpService::CloudResourceManager,
            GcpService::ArtifactRegistry,
            GcpService::Storage,
            GcpService::SecretManager,
        ] {
            client = client.with_base_url(service, server.uri());
        }
        client
    }

    fn binding(role: &str, member: &str) -> Value {
        json!({ "role": role, "members": [member] })
    }

    fn ok() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({}))
    }

    fn done_operation() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({ "name": "operations/x", "done": true }))
    }

    fn policy(bindings: &[Value]) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({ "version": 3, "bindings": bindings }))
    }

    async fn mount(server: &MockServer, verb: &str, at: &str, response: ResponseTemplate) {
        Mock::given(method(verb))
            .and(path(at))
            .respond_with(response)
            .mount(server)
            .await;
    }

    async fn mount_once(
        server: &MockServer,
        verb: &str,
        at: &str,
        body: Value,
        response: ResponseTemplate,
    ) {
        Mock::given(method(verb))
            .and(path(at))
            .and(body_partial_json(body))
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_project_number(server: &MockServer) {
        mount(
            server,
            "GET",
            "/v3/projects/example-staging",
            ResponseTemplate::new(200).set_body_json(json!({ "name": "projects/123456" })),
        )
        .await;
    }

    fn impersonation() -> Value {
        binding(
            "roles/iam.workloadIdentityUser",
            &principal_set("123456", "staging-deploy"),
        )
    }

    #[test]
    fn the_provider_condition_pins_repository_environment_and_main() {
        assert_eq!(
            attribute_condition(&ship()),
            "assertion.repository == 'example-org/navigator' && \
             assertion.environment == 'staging-deploy' && assertion.ref == 'refs/heads/main'"
        );
        assert_eq!(
            principal_set("123456", "staging-deploy"),
            "principalSet://iam.googleapis.com/projects/123456/locations/global/\
             workloadIdentityPools/navigator-release/attribute.environment/staging-deploy"
        );
    }

    #[test]
    fn values_that_could_widen_the_condition_are_refused() {
        type Field = fn(&mut ReleaseShipConfig) -> &mut String;
        let cases: [(Field, &str); 5] = [
            (|s| &mut s.deployment, "../other"),
            (|s| &mut s.repository, "example-org"),
            (|s| &mut s.repository, "example-org/nav' || true || '"),
            (|s| &mut s.environment, "staging' || true || '"),
            (|s| &mut s.environment, ""),
        ];
        for (field, value) in cases {
            let mut candidate = ship();
            *field(&mut candidate) = value.into();
            let error = candidate.validate().expect_err(value);
            assert!(matches!(error, SetupError::InvalidFlag { .. }), "{error}");
        }
        ship().validate().unwrap();
    }

    #[tokio::test]
    async fn an_invalid_ship_flag_stops_setup_before_any_gcp_call() {
        let client = GcpClient::new(Arc::new(StaticToken("t".into()))).with_dry_run();
        let mut config = config();
        config.public_base_url = Some("https://www.example.test".into());
        config.release_ship = Some(ReleaseShipConfig {
            environment: "bad env".into(),
            ..ship()
        });
        let error = super::super::run(&client, PROJECT, &config)
            .await
            .expect_err("a space cannot reach a CEL condition");
        assert!(error.to_string().contains("--ship-environment"), "{error}");
        assert!(client.recorded_calls().is_empty());
    }

    /// A fresh project: every read is empty, so every binding is written once,
    /// and the project policy is read and written at version 3.
    // One mock per grant the stage writes; splitting it would hide whether a
    // fresh project gets every binding from the same run.
    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn a_fresh_project_gets_every_grant_and_the_provider() {
        let server = MockServer::start().await;
        mount_project_number(&server).await;
        mount_once(
            &server,
            "POST",
            "/v1/projects/example-staging/roles",
            json!({
                "roleId": SIGNING_IAM_READER_ROLE_ID,
                "role": { "includedPermissions": [SIGNING_IAM_READER_PERMISSION], "stage": "GA" }
            }),
            ok(),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/example-staging:getIamPolicy"))
            .and(body_partial_json(
                json!({ "options": { "requestedPolicyVersion": 3 } }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "etag": "e1" })))
            .expect(4)
            .mount(&server)
            .await;
        for role in PROJECT_ROLES.iter().copied().chain([CUSTOM_ROLE]) {
            mount_once(
                &server,
                "POST",
                "/v1/projects/example-staging:setIamPolicy",
                json!({ "policy": { "version": 3, "etag": "e1", "bindings": [binding(role, MEMBER)] } }),
                ok(),
            )
            .await;
        }
        mount(&server, "GET", &format!("{REPO_PATH}:getIamPolicy"), ok()).await;
        mount_once(
            &server,
            "POST",
            &format!("{REPO_PATH}:setIamPolicy"),
            json!({ "policy": { "bindings": [binding(READER_ROLE, MEMBER)] } }),
            ok(),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/storage/v1/b/example-staging-assets/iam"))
            .and(query_param("optionsRequestedPolicyVersion", "3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "bindings": [] })))
            .mount(&server)
            .await;
        mount_once(
            &server,
            "PUT",
            "/storage/v1/b/example-staging-assets/iam",
            json!({ "version": 3, "bindings": [binding(ASSETS_ROLE, MEMBER)] }),
            ok(),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/example-staging/secrets"))
            .and(query_param(
                "secretId",
                "navigator-deployment-config-example-a",
            ))
            .respond_with(ok())
            .expect(1)
            .mount(&server)
            .await;
        mount(&server, "GET", &format!("{SECRET_PATH}:getIamPolicy"), ok()).await;
        mount_once(
            &server,
            "POST",
            &format!("{SECRET_PATH}:setIamPolicy"),
            json!({ "policy": { "bindings": [binding(CONFIG_SECRET_ROLE, MEMBER)] } }),
            ok(),
        )
        .await;
        Mock::given(method("POST"))
            .and(path(
                "/v1/projects/example-staging/locations/global/workloadIdentityPools",
            ))
            .and(query_param("workloadIdentityPoolId", RELEASE_WIF_POOL_ID))
            .respond_with(done_operation())
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(PROVIDERS_PATH))
            .and(query_param(
                "workloadIdentityPoolProviderId",
                RELEASE_WIF_PROVIDER_ID,
            ))
            .and(body_partial_json(json!({
                "oidc": { "issuerUri": GITHUB_OIDC_ISSUER },
                "attributeMapping": {
                    "google.subject": "assertion.sub",
                    "attribute.repository": "assertion.repository",
                    "attribute.environment": "assertion.environment"
                },
                "attributeCondition": attribute_condition(&ship())
            })))
            .respond_with(done_operation())
            .expect(1)
            .mount(&server)
            .await;
        mount(&server, "POST", SA_POLICY_PATH, ok()).await;
        mount_once(
            &server,
            "POST",
            &SA_POLICY_PATH.replace(":getIamPolicy", ":setIamPolicy"),
            json!({ "policy": { "bindings": [impersonation()] } }),
            ok(),
        )
        .await;

        ensure(
            &client_for(&server),
            PROJECT,
            &config(),
            &ship(),
            "example-staging-assets",
        )
        .await
        .unwrap();
    }

    /// The re-run: every resource exists and every binding is in place, so
    /// setup only reads — no setIamPolicy, PUT, PATCH, or undelete.
    #[tokio::test]
    async fn a_converged_project_is_read_and_never_written() {
        let server = MockServer::start().await;
        mount_project_number(&server).await;
        let conflict = || ResponseTemplate::new(409).set_body_string("ALREADY_EXISTS");
        mount(
            &server,
            "POST",
            "/v1/projects/example-staging/roles",
            conflict(),
        )
        .await;
        mount(
            &server,
            "GET",
            "/v1/projects/example-staging/roles/navigatorSigningIamReader",
            ResponseTemplate::new(200).set_body_json(json!({
                "includedPermissions": [SIGNING_IAM_READER_PERMISSION],
                "etag": "r1"
            })),
        )
        .await;
        let project_bindings: Vec<Value> = PROJECT_ROLES
            .iter()
            .copied()
            .chain([CUSTOM_ROLE])
            .map(|role| binding(role, MEMBER))
            .collect();
        mount(
            &server,
            "POST",
            "/v1/projects/example-staging:getIamPolicy",
            policy(&project_bindings),
        )
        .await;
        mount(
            &server,
            "GET",
            &format!("{REPO_PATH}:getIamPolicy"),
            policy(&[binding(READER_ROLE, MEMBER)]),
        )
        .await;
        mount(
            &server,
            "GET",
            "/storage/v1/b/example-staging-assets/iam",
            policy(&[binding(ASSETS_ROLE, MEMBER)]),
        )
        .await;
        mount(
            &server,
            "POST",
            "/v1/projects/example-staging/secrets",
            conflict(),
        )
        .await;
        mount(
            &server,
            "GET",
            &format!("{SECRET_PATH}:getIamPolicy"),
            policy(&[binding(CONFIG_SECRET_ROLE, MEMBER)]),
        )
        .await;
        mount(
            &server,
            "POST",
            "/v1/projects/example-staging/locations/global/workloadIdentityPools",
            conflict(),
        )
        .await;
        mount(&server, "POST", PROVIDERS_PATH, conflict()).await;
        mount(
            &server,
            "GET",
            PROVIDER_PATH,
            ResponseTemplate::new(200).set_body_json(provider_body(&ship())),
        )
        .await;
        mount(&server, "POST", SA_POLICY_PATH, policy(&[impersonation()])).await;
        for verb in ["PUT", "PATCH"] {
            Mock::given(method(verb))
                .respond_with(ResponseTemplate::new(500))
                .expect(0)
                .mount(&server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path_regex(":(setIamPolicy|undelete)$"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        ensure(
            &client_for(&server),
            PROJECT,
            &config(),
            &ship(),
            "example-staging-assets",
        )
        .await
        .unwrap();
    }

    /// A conditional grant of the same role is narrower than the one the ship
    /// needs, so it does not satisfy the ensure, and the write keeps it.
    #[tokio::test]
    async fn a_conditional_binding_does_not_count_and_survives_the_write() {
        let server = MockServer::start().await;
        let conditional = json!({
            "role": "roles/secretmanager.viewer",
            "members": [MEMBER],
            "condition": { "title": "narrow", "expression": "false" }
        });
        mount(
            &server,
            "POST",
            "/v1/projects/example-staging:getIamPolicy",
            policy(std::slice::from_ref(&conditional)),
        )
        .await;
        mount_once(
            &server,
            "POST",
            "/v1/projects/example-staging:setIamPolicy",
            json!({ "policy": { "version": 3, "bindings": [
                conditional,
                binding("roles/secretmanager.viewer", MEMBER),
            ] } }),
            ok(),
        )
        .await;
        let outcome = ensure_project_member(
            &client_for(&server),
            PROJECT,
            "roles/secretmanager.viewer",
            MEMBER,
        )
        .await
        .unwrap();
        assert_eq!(outcome, BindingOutcome::Added);
    }

    #[tokio::test]
    async fn a_deleted_role_without_the_permission_is_restored_and_patched() {
        let server = MockServer::start().await;
        mount(
            &server,
            "POST",
            "/v1/projects/example-staging/roles",
            ResponseTemplate::new(409),
        )
        .await;
        mount(
            &server,
            "GET",
            "/v1/projects/example-staging/roles/navigatorSigningIamReader",
            ResponseTemplate::new(200).set_body_json(json!({
                "deleted": true,
                "etag": "r1",
                "includedPermissions": []
            })),
        )
        .await;
        mount_once(
            &server,
            "POST",
            "/v1/projects/example-staging/roles/navigatorSigningIamReader:undelete",
            json!({ "etag": "r1" }),
            ok(),
        )
        .await;
        Mock::given(method("PATCH"))
            .and(path(
                "/v1/projects/example-staging/roles/navigatorSigningIamReader",
            ))
            .and(query_param("updateMask", "includedPermissions"))
            .and(body_partial_json(json!({
                "includedPermissions": [SIGNING_IAM_READER_PERMISSION]
            })))
            .respond_with(ok())
            .expect(1)
            .mount(&server)
            .await;
        let name = ensure_signing_iam_reader_role(&client_for(&server), PROJECT)
            .await
            .unwrap();
        assert_eq!(name, CUSTOM_ROLE);
    }

    /// A provider left on another condition converges onto the flags after
    /// its pre-image is saved, and is read back to prove it.
    #[tokio::test]
    async fn a_drifted_provider_is_patched_and_verified() {
        let server = MockServer::start().await;
        mount(&server, "POST", PROVIDERS_PATH, ResponseTemplate::new(409)).await;
        let mut drifted = provider_body(&ship());
        drifted["attributeCondition"] = json!("assertion.repository == 'example-org/navigator'");
        Mock::given(method("GET"))
            .and(path(PROVIDER_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(drifted))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        mount(
            &server,
            "GET",
            PROVIDER_PATH,
            ResponseTemplate::new(200).set_body_json(provider_body(&ship())),
        )
        .await;
        Mock::given(method("PATCH"))
            .and(path(PROVIDER_PATH))
            .and(query_param(
                "updateMask",
                "attributeCondition,attributeMapping,oidc",
            ))
            .and(body_partial_json(
                json!({ "attributeCondition": attribute_condition(&ship()) }),
            ))
            .respond_with(done_operation())
            .expect(1)
            .mount(&server)
            .await;
        let outcome = ensure_provider(&client_for(&server), PROJECT, &ship())
            .await
            .unwrap();
        assert_eq!(outcome, EnsureOutcome::AlreadyExists);
    }

    /// The whole pipeline, dry-run, with the ship flags in hub mode: the
    /// registry grant lands in the hub, everything else in the environment.
    #[tokio::test]
    async fn the_dry_run_pipeline_records_the_ship_stage_after_log_retention() {
        let mut client = GcpClient::new(Arc::new(StaticToken("t".into())));
        for service in [
            GcpService::ServiceUsage,
            GcpService::Compute,
            GcpService::Storage,
            GcpService::ArtifactRegistry,
            GcpService::Iam,
            GcpService::CloudResourceManager,
            GcpService::CloudKms,
            GcpService::SecretManager,
        ] {
            client = client.with_base_url(service, "http://127.0.0.1:1");
        }
        let client = client.with_dry_run();
        let mut config = config();
        config.public_base_url = Some("https://www.example.test".into());
        config.images_project_id = Some(super::super::tenants::HUB_PROJECT_ID.into());
        super::super::run(&client, PROJECT, &config).await.unwrap();

        let calls = client.recorded_calls();
        let start = calls
            .iter()
            .position(|call| call.url.ends_with("/v1/projects/example-staging/roles"))
            .expect("the ship stage creates the custom role");
        let stage = &calls[start..calls.len() - 2];
        // Role + 4 x project get/set + hub reader get/set + bucket get/put +
        // secret create + secret get/set + pool + provider + impersonation
        // get/set = 20.
        assert_eq!(stage.len(), 20, "{stage:?}");
        let plan = stage
            .iter()
            .map(|c| {
                format!(
                    "{} {} {}",
                    c.method,
                    c.url,
                    c.body.as_deref().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        for needle in PROJECT_ROLES.iter().copied().chain([
            CUSTOM_ROLE,
            "/projects/ghcr/locations/example-region/repositories/navigator:setIamPolicy",
            "roles/artifactregistry.reader",
            "PUT http://127.0.0.1:1/storage/v1/b/example-staging-assets/iam",
            "roles/storage.objectViewer",
            "secrets?secretId=navigator-deployment-config-example-a",
            "navigator-deployment-config-example-a:setIamPolicy",
            "roles/secretmanager.secretAccessor",
            "workloadIdentityPoolId=navigator-release",
            "assertion.environment == 'staging-deploy'",
            "workloadIdentityPools/navigator-release/attribute.environment/staging-deploy",
        ]) {
            assert!(plan.contains(needle), "ship stage omits {needle}:\n{plan}");
        }
        assert!(!plan.contains(":addVersion"), "no configuration version");
        assert!(
            !plan.contains("navigator-web@"),
            "the protected web service account is never written:\n{plan}"
        );
    }
}

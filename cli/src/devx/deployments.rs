//! The deployment tree: plaintext coordinates beside SOPS-encrypted key
//! material.
//!
//! ```text
//! deployments/<name>/config.toml       coordinates, plaintext, reviewable
//! deployments/<name>/secrets.enc.yaml  key material, SOPS, per-deployment KMS key
//! .sops.yaml                           which key encrypts which deployment
//! ```
//!
//! ## Where the tree is
//!
//! **Not in this repository.** The real rows live in a private repository,
//! beside the workflow that rolls them, which this one does not name.
//! This one is public and accepts pull requests from strangers, so a workflow
//! here holding a credential that reaches the cluster is an attack surface the
//! split removes; the tree moved because the workflow needs it on disk, not
//! because per-value KMS ciphertext needed hiding.
//!
//! [`root`] is how a command finds it: `--deployments-dir`, then
//! `NAVIGATOR_DEPLOYMENTS_DIR`, then the workspace. The workspace fallback is
//! what the synthetic tree under `cli/tests/fixtures/deployment-tree/` is NOT
//! — the fixture is passed explicitly, and it exists so the gates below keep
//! running here. See its README.
//!
//! `ops deployments import <name>` (alias `--publish`) stores a row's
//! coordinates and secret key inventories in the installation's Secret
//! Manager, once. From then on that record is the row's only source;
//! `ops ship --project-id` reads it without a local tree. See
//! `deployment_config`.
//!
//! ## What this module will and will not read
//!
//! Two entry points, deliberately asymmetric. [`Deployment::load`] reads the
//! plaintext coordinates and the *key names* of the encrypted file — never a
//! value, never a `sops` invocation, no KMS call, no credential. Every audit,
//! parity check, and `--dry-run` runs on that. Only [`apply`] decrypts, and
//! only after the names-only check has already passed.
//!
//! ## Why `sops` is a shell-out
//!
//! `sops` is invoked the way this CLI already invokes `gcloud`, `kubectl`,
//! `helm`, and `docker`. The Rust-owns-the-workspace invariant governs
//! Navigator's own automation, not every dependency it drives. The Rust
//! reimplementation `rops` was rejected on a concrete capability gap rather
//! than on maturity: it implements age and AWS KMS only, so it cannot express
//! the per-deployment Cloud KMS keys this design is built on.
//!
//! ## Rotation
//!
//! Re-encrypting a file rotates the data key. It revokes nothing: every prior
//! ciphertext stays readable to anyone holding repo history and the KMS key,
//! because the key decrypts history and not just `HEAD`. **A rotation is a
//! rotation at the provider, followed by re-encrypting here.** The file edit
//! alone is a no-op against an attacker who already has the old bytes. See
//! `docs/deployment-secrets.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, ensure, Context, Result};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use serde::Deserialize;
use store::deployment::{applicable_web_requirements, Requirement};
use store::DeploymentEnvironment;

use super::gcp;
use super::ship;

/// The tree's root, relative to the workspace.
pub const TREE: &str = "deployments";
/// Plaintext coordinates. Reviewable in a diff, greppable, CI-checkable.
pub const CONFIG_FILE: &str = "config.toml";
/// Key material, encrypted per value against this deployment's own KMS key.
pub const SECRETS_FILE: &str = "secrets.enc.yaml";
/// The creation rules deciding which KMS key encrypts which deployment. Sits
/// beside the tree rather than inside it, because a rule's `path_regex` is
/// written relative to the directory `sops` runs from.
pub const SOPS_CONFIG: &str = ".sops.yaml";

/// Names the directory the tree lives in, for a checkout that is not the
/// workspace. Read by [`root`] when no explicit path is passed.
pub const ROOT_ENV: &str = "NAVIGATOR_DEPLOYMENTS_DIR";

/// The coordinate every other coordinate is scoped by.
const PROJECT_ID: &str = "NAVIGATOR_GCP_PROJECT_ID";

/// `sops` marks each encrypted scalar with this prefix. A value that does not
/// carry it is plaintext sitting in the repository.
const ENCRYPTED_PREFIX: &str = "ENC[";
/// The metadata block `sops` appends to every file it encrypts.
const SOPS_METADATA_KEY: &str = "sops";

/// Apply the deployment Secret without creating kubectl's client-side
/// `last-applied-configuration` annotation, which would duplicate every
/// plaintext value in object metadata.
const WEB_SECRET_APPLY_ARGS: &[&str] = &["apply", "--filename", "-", "--server-side"];
const SERVICE_SECRET_APPLY_ARGS: &[&str] = &["apply", "--filename", "-", "--server-side"];

/// The PostgreSQL client logins' inputs: one password per least-privilege
/// role (`k8s/postgres/access/roles.sql`) and the CA bundle that verifies the
/// server certificate. Optional as a set in the PostgreSQL service file.
const POSTGRES_CLIENT_KEYS: &[&str] = &[
    "NAVIGATOR_POSTGRES_CA_CERT",
    "NAVIGATOR_POSTGRES_APP_PASSWORD",
    "NAVIGATOR_POSTGRES_MIGRATOR_PASSWORD",
    "NAVIGATOR_POSTGRES_BACKUP_PASSWORD",
];

/// One mounted-file Secret per client login, each with its own `password`
/// and the shared `ca.crt`, so a workload mounts only its own login.
const POSTGRES_CLIENT_SECRETS: &[(&str, &str)] = &[
    ("navigator-postgres-app", "NAVIGATOR_POSTGRES_APP_PASSWORD"),
    (
        "navigator-postgres-migrator",
        "NAVIGATOR_POSTGRES_MIGRATOR_PASSWORD",
    ),
    (
        "navigator-postgres-backup",
        "NAVIGATOR_POSTGRES_BACKUP_PASSWORD",
    ),
];

/// The DBOS executor's login (`navigator_dbos`, which owns only the `dbos`
/// schema). Optional in the PostgreSQL service file; it shares the client
/// logins' CA bundle, so it needs that set too.
const POSTGRES_DBOS_KEYS: &[&str] = &["NAVIGATOR_POSTGRES_DBOS_PASSWORD"];

/// The mounted-file Secret holding the DBOS login's `password` and `ca.crt`.
const POSTGRES_DBOS_SECRET: &str = "navigator-postgres-dbos";

/// Forgejo's `OpenID` Connect client secret at the installation's issuer.
/// Optional as a set: without it Forgejo has no external sign-in. Its public
/// client id is the `NAVIGATOR_FORGEJO_OIDC_CLIENT_ID` coordinate.
const FORGEJO_OIDC_KEYS: &[&str] = &["NAVIGATOR_FORGEJO_OIDC_CLIENT_SECRET"];

/// The deployment coordinate naming Forgejo's public OIDC client id.
const FORGEJO_OIDC_CLIENT_ID: &str = "NAVIGATOR_FORGEJO_OIDC_CLIENT_ID";

const FORGEJO_ACTIONS_ENABLED: &str = "NAVIGATOR_FORGEJO_ACTIONS_ENABLED";
const FORGEJO_SIGNING_KEYS: &[&str] = &[
    "NAVIGATOR_FORGEJO_SIGNING_PRIVATE_KEY",
    "NAVIGATOR_FORGEJO_SIGNING_PUBLIC_KEY",
];
const FORGEJO_SIGNING_NAME: &str = "NAVIGATOR_FORGEJO_SIGNING_NAME";
const FORGEJO_SIGNING_EMAIL: &str = "NAVIGATOR_FORGEJO_SIGNING_EMAIL";

fn forgejo_actions_enabled(deployment: &Deployment) -> Result<bool> {
    match deployment
        .coordinates
        .get(FORGEJO_ACTIONS_ENABLED)
        .map(String::as_str)
    {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => bail!("{FORGEJO_ACTIONS_ENABLED} must be true or false"),
    }
}

fn forgejo_signing_identity(deployment: &Deployment) -> Result<Option<(&str, &str)>> {
    let signing = FORGEJO_SIGNING_KEYS
        .iter()
        .any(|key| deployment.forgejo_encrypted_keys.contains(*key));
    ensure!(
        !forgejo_actions_enabled(deployment)? || signing,
        "enabling Forgejo Actions requires the instance signing key pair and identity"
    );
    if !signing {
        return Ok(None);
    }
    ensure!(
        FORGEJO_SIGNING_KEYS
            .iter()
            .all(|key| deployment.forgejo_encrypted_keys.contains(*key)),
        "Forgejo instance signing requires both private and public keys"
    );
    let coordinate = |key| -> Result<&str> {
        let value = deployment
            .coordinates
            .get(key)
            .map(String::as_str)
            .unwrap_or_default();
        ensure!(
            !value.trim().is_empty() && !value.contains(['\n', '\r', '\0', ';', '#', '[', ']']),
            "{key} must be a nonempty single-line signing identity"
        );
        Ok(value)
    };
    Ok(Some((
        coordinate(FORGEJO_SIGNING_NAME)?,
        coordinate(FORGEJO_SIGNING_EMAIL)?,
    )))
}

/// Opt-in service whose credentials are projected only as a mounted file Secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceSecrets {
    Postgres,
    Forgejo,
}

impl ServiceSecrets {
    fn file_name(self) -> &'static str {
        match self {
            Self::Postgres => "postgres-secrets.enc.yaml",
            Self::Forgejo => "forgejo-secrets.enc.yaml",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Forgejo => "forgejo",
        }
    }

    fn keys(self) -> &'static [&'static str] {
        match self {
            Self::Postgres => &[
                "NAVIGATOR_POSTGRES_PASSWORD",
                "NAVIGATOR_POSTGRES_TLS_CERTIFICATE",
                "NAVIGATOR_POSTGRES_TLS_PRIVATE_KEY",
            ],
            Self::Forgejo => &[
                "NAVIGATOR_FORGEJO_DATABASE_PASSWORD",
                "NAVIGATOR_FORGEJO_SECRET_KEY",
                "NAVIGATOR_FORGEJO_INTERNAL_TOKEN",
                "NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET",
                "NAVIGATOR_FORGEJO_POSTGRES_CA_CERT",
            ],
        }
    }

    /// Key sets a service file may add as a whole. A set is either complete
    /// or absent, so a file written before the set existed stays valid.
    fn optional_key_sets(self) -> &'static [&'static [&'static str]] {
        match self {
            Self::Postgres => &[POSTGRES_CLIENT_KEYS, POSTGRES_DBOS_KEYS],
            Self::Forgejo => &[FORGEJO_OIDC_KEYS, FORGEJO_SIGNING_KEYS],
        }
    }

    /// Every key the service file may carry.
    fn allowed_keys(self) -> impl Iterator<Item = &'static str> {
        self.keys()
            .iter()
            .chain(self.optional_key_sets().iter().flat_map(|set| set.iter()))
            .copied()
    }

    fn targets(self, keys: &BTreeSet<String>) -> Vec<&'static str> {
        match self {
            Self::Postgres => {
                let mut targets = vec!["navigator-postgres-auth", "navigator-postgres-tls"];
                if keys.contains(POSTGRES_CLIENT_KEYS[0]) {
                    targets.extend(POSTGRES_CLIENT_SECRETS.iter().map(|(name, _)| *name));
                }
                if keys.contains(POSTGRES_DBOS_KEYS[0]) {
                    targets.push(POSTGRES_DBOS_SECRET);
                }
                targets
            }
            Self::Forgejo => {
                let mut targets = vec![
                    "navigator-forgejo-config",
                    "navigator-forgejo-postgres-ca",
                    "navigator-forgejo-database",
                ];
                if keys.contains(FORGEJO_OIDC_KEYS[0]) {
                    targets.push("navigator-forgejo-oidc");
                }
                if keys.contains(FORGEJO_SIGNING_KEYS[0]) {
                    targets.push("navigator-forgejo-signing");
                }
                targets
            }
        }
    }

    fn encrypted_keys(self, deployment: &Deployment) -> &BTreeSet<String> {
        match self {
            Self::Postgres => &deployment.postgres_encrypted_keys,
            Self::Forgejo => &deployment.forgejo_encrypted_keys,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    kms_key: String,
    #[serde(default = "provisioned_by_default")]
    provisioned: bool,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

/// A deployment is provisioned unless it says otherwise.
///
/// The default is the safe one: forgetting the key on a real row makes it
/// eligible for every gate, where forgetting it on an unprovisioned row would
/// silently exempt it.
const fn provisioned_by_default() -> bool {
    true
}

/// One deployment as the repository describes it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deployment {
    pub name: String,
    /// The Cloud KMS key `sops` encrypts this deployment's key material
    /// against, in this deployment's own project.
    pub kms_key: String,
    /// Whether the cloud resources this row describes actually exist yet.
    ///
    /// `false` means the tree describes a deployment nobody has run
    /// `ops gcp setup` for: its project, KMS key, buckets, OAuth client, and
    /// Surreal instance are all still to be created, so its coordinates are
    /// blank and it has no `secrets.enc.yaml` to encrypt against a key that
    /// does not exist.
    ///
    /// This is a declared state rather than an inferred one. A row with empty
    /// coordinates is indistinguishable from a row somebody broke, and the
    /// completeness gates cannot tell the difference on their own — so an
    /// unprovisioned row says so in one greppable line, the gates skip exactly
    /// those rows, and `ops ship` refuses one outright rather than failing
    /// somewhere deep in a `kubectl apply`.
    pub provisioned: bool,
    /// Plaintext, non-secret deployment coordinates.
    pub coordinates: BTreeMap<String, String>,
    /// The key *names* the encrypted file carries. A `BTreeSet<String>` and
    /// not a map on purpose: there is nowhere for a value to live, so no
    /// audit path can print one even by mistake.
    pub encrypted_keys: BTreeSet<String>,
    /// PostgreSQL credentials; never part of `web`'s env Secret.
    pub postgres_encrypted_keys: BTreeSet<String>,
    /// Forgejo credentials; never part of `web`'s env Secret.
    pub forgejo_encrypted_keys: BTreeSet<String>,
}

impl Deployment {
    /// Read `<root>/deployments/<name>/`. Fails closed on a missing file, a
    /// missing project coordinate, or an encrypted file that is not encrypted.
    pub fn load(root: &Path, name: &str) -> Result<Self> {
        let dir = root.join(TREE).join(name);
        if !dir.is_dir() {
            bail!(
                "no deployment `{name}` in {}/. Known deployments: {}",
                TREE,
                names(root)?.join(", ")
            );
        }
        let config_path = dir.join(CONFIG_FILE);
        let config: ConfigFile = toml::from_str(
            &fs::read_to_string(&config_path)
                .with_context(|| format!("read {}", config_path.display()))?,
        )
        .with_context(|| format!("parse {}", config_path.display()))?;

        if !config.env.contains_key(PROJECT_ID) {
            bail!(
                "{} must set {PROJECT_ID}: every coordinate and every Secret Manager write is \
                 scoped by the deployment's own project",
                config_path.display()
            );
        }

        // An unprovisioned row has no `secrets.enc.yaml`, and cannot: `sops`
        // encrypts against a KMS key that does not exist yet. Reading it as an
        // empty set is the honest answer — the row genuinely supplies no key
        // material — and every gate that cares checks `provisioned` first.
        let secrets_path = dir.join(SECRETS_FILE);
        let encrypted_keys = match fs::read_to_string(&secrets_path) {
            Ok(body) => encrypted_key_names(&body)
                .with_context(|| format!("read the key names of {}", secrets_path.display()))?,
            Err(error) if !config.provisioned && error.kind() == std::io::ErrorKind::NotFound => {
                BTreeSet::new()
            }
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", secrets_path.display()))
            }
        };

        // Service credentials are provisioned only when the corresponding
        // opt-in service is selected. Separate files ensure applying one
        // service never decrypts the other's credentials.
        let postgres_encrypted_keys =
            read_optional_encrypted_key_names(&dir.join(ServiceSecrets::Postgres.file_name()))?;
        let forgejo_encrypted_keys =
            read_optional_encrypted_key_names(&dir.join(ServiceSecrets::Forgejo.file_name()))?;

        Ok(Self {
            name: name.to_owned(),
            kms_key: config.kms_key,
            provisioned: config.provisioned,
            coordinates: config.env,
            encrypted_keys,
            postgres_encrypted_keys,
            forgejo_encrypted_keys,
        })
    }

    /// The GCP project that owns this deployment's Secret Manager and KMS key.
    #[must_use]
    pub fn project_id(&self) -> &str {
        self.coordinates
            .get(PROJECT_ID)
            .expect("load rejects a config without a project id")
    }

    /// Every key name this deployment supplies to a running pod: plaintext
    /// coordinates, encrypted key material, and the keys the Deployment
    /// manifest supplies as inline env rather than through the Secret.
    #[must_use]
    pub fn supplied_keys(&self) -> BTreeSet<String> {
        self.coordinates
            .keys()
            .chain(self.encrypted_keys.iter())
            .cloned()
            .chain(ship::INLINE_ENV_WEB_KEYS.iter().map(ToString::to_string))
            .collect()
    }
}

/// The directory that CONTAINS `deployments/` — which is also the directory
/// `.sops.yaml` sits in, because a creation rule's `path_regex` is written
/// relative to it.
///
/// Three sources, in priority order: an explicit `--deployments-dir`, the
/// `NAVIGATOR_DEPLOYMENTS_DIR` environment variable, and the discovered
/// workspace root. The first two exist because the tree does not have to live
/// beside the source that reads it. `orchestrate::workspace_root` walks up for
/// a directory holding both `Cargo.toml` and `k8s/`; a checkout that carries
/// the deployment tree and nothing else has neither, so without an override
/// every read of the tree bails there rather than at the tree.
///
/// Nothing is inferred from the process environment beyond that one variable,
/// and the variable names a *location*, never a deployment — `--deployment`
/// stays explicit for the same reason it always has.
pub fn root(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = explicit {
        return holding_the_tree(dir, "--deployments-dir");
    }
    if let Some(value) = env::var_os(ROOT_ENV) {
        return holding_the_tree(Path::new(&value), ROOT_ENV);
    }
    let workspace = super::orchestrate::workspace_root()?;
    if workspace.join(TREE).is_dir() {
        return Ok(workspace);
    }
    // A bare `no deployment <name>` from `Deployment::load` would describe the
    // wrong problem, and `names` would fail on a `read_dir` of a path that does
    // not exist.
    bail!(
        "no `{TREE}/` in the workspace at {}. Run this from a checkout of the Navigator \
         repository, which holds the tree, or pass `--deployments-dir` (or set {ROOT_ENV}) to \
         point at one",
        workspace.display()
    )
}

/// Accept `dir` only if the tree is actually under it.
///
/// The failure this spends a branch on is being handed the tree itself. It is
/// the reading the flag's name invites, it produces a `no deployment <name> in
/// deployments/` error that describes the wrong problem, and the fix — pass
/// the parent — is not derivable from that message.
fn holding_the_tree(dir: &Path, source: &str) -> Result<PathBuf> {
    if dir.join(TREE).is_dir() {
        return Ok(dir.to_path_buf());
    }
    if dir.file_name().is_some_and(|name| name == TREE) {
        let parent = dir
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| ".".to_owned(), |parent| parent.display().to_string());
        bail!(
            "{source} is {}, the `{TREE}` tree itself. It wants the directory CONTAINING the \
             tree, which is also where `.sops.yaml` sits: pass {parent} instead",
            dir.display()
        );
    }
    bail!(
        "{source} is {}, which has no `{TREE}/` directory in it",
        dir.display()
    )
}

/// Every deployment directory in the tree, sorted.
pub fn names(root: &Path) -> Result<Vec<String>> {
    let tree = root.join(TREE);
    let mut names = Vec::new();
    for entry in
        fs::read_dir(&tree).with_context(|| format!("read the tree at {}", tree.display()))?
    {
        let entry = entry.context("read a deployment directory entry")?;
        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// The key names of a SOPS-encrypted YAML document, and the guard that a
/// plaintext value can never be committed as one.
///
/// Three things must hold, and each has its own failure mode:
///
/// 1. The `sops` metadata block is present. Without it the file was never
///    encrypted at all — the accident of writing values and forgetting to run
///    `sops --encrypt`.
/// 2. Every value is `ENC[…]`. This is the per-value encryption the design
///    turns on: a whole-file envelope would produce a single `data` blob
///    instead, which makes every rotation an opaque one-line diff.
/// 3. Every key is an environment-variable name. A blob file's `data` key
///    fails this too, so the two guards reinforce each other.
pub fn encrypted_key_names(document: &str) -> Result<BTreeSet<String>> {
    let parsed: BTreeMap<String, serde_yaml::Value> =
        serde_yaml::from_str(document).context("parse the encrypted YAML document")?;

    let mut names = BTreeSet::new();
    let mut has_metadata = false;
    for (key, value) in parsed {
        if key == SOPS_METADATA_KEY {
            has_metadata = true;
            continue;
        }
        if !is_env_var_name(&key) {
            bail!(
                "`{key}` is not an environment-variable name. Key material is encrypted per \
                 value, one entry per variable; a whole-file envelope (a single `data` blob) \
                 makes every rotation an opaque one-line diff"
            );
        }
        let encrypted = value
            .as_str()
            .is_some_and(|value| value.starts_with(ENCRYPTED_PREFIX));
        if !encrypted {
            bail!(
                "`{key}` is not encrypted: its value does not start with `{ENCRYPTED_PREFIX}`. \
                 Run `sops --encrypt --in-place` before committing — a plaintext secret in the \
                 repository cannot be un-committed, only rotated at the provider"
            );
        }
        names.insert(key);
    }

    if !has_metadata {
        bail!(
            "the document carries no `{SOPS_METADATA_KEY}` metadata block, so it was never \
             encrypted"
        );
    }
    Ok(names)
}

/// Read a service SOPS file's key names without decrypting it. The file is
/// optional until an operator enables that service for a deployment.
fn read_optional_encrypted_key_names(path: &Path) -> Result<BTreeSet<String>> {
    match fs::read_to_string(path) {
        Ok(body) => encrypted_key_names(&body)
            .with_context(|| format!("read the key names of {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// `^[A-Z][A-Z0-9_]*$` — the same shape `.sops.yaml`'s `encrypted_regex`
/// selects values by, so a key this refuses is also a key `sops` would have
/// left in plaintext.
pub(super) fn is_env_var_name(key: &str) -> bool {
    let mut characters = key.chars();
    characters.next().is_some_and(|c| c.is_ascii_uppercase())
        && characters.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// The boot requirements this deployment does not satisfy.
///
/// Not "every deployment carries every key": `applicable_web_requirements`
/// scopes specialized requirements to explicitly marked deployment rows. A
/// deployment is measured against the requirements that apply *to it*.
///
/// Names only. Nothing here decrypts, so this runs in CI with no credential
/// and no network call.
pub fn unsatisfied_requirements(deployment: &Deployment) -> Result<Vec<Requirement>> {
    let supplied = deployment.supplied_keys();
    let get = |key: &str| -> Option<String> {
        // A coordinate answers with its real value — `NAVIGATOR_GCP_PROJECT_ID`
        // decides which project-scoped requirements apply. Everything else
        // answers with presence: the file's key names are all this may see,
        // and `encrypted_key_names` has already refused an empty one.
        deployment
            .coordinates
            .get(key)
            .cloned()
            .or_else(|| supplied.contains(key).then(|| "1".to_owned()))
    };

    // Every hosted deployment runs the production profile, so the CI-harness
    // relaxation cannot apply and every integration-tier requirement counts.
    Ok(
        applicable_web_requirements(DeploymentEnvironment::Production, &get)?
            .into_iter()
            .filter(|requirement| {
                !requirement
                    .any_of
                    .iter()
                    .any(|alternative| alternative.iter().all(|key| supplied.contains(*key)))
            })
            .collect(),
    )
}

/// The keys an enabled inbound-summary lane boots with that this row does not
/// supply. Empty when the row leaves the lane off.
///
/// The lane is gated by `NAVIGATOR_SUMMARY_ENABLED`, a value rather than a
/// presence, so it cannot ride `WEB_REQUIREMENTS`' presence triggers: a row
/// that writes `"false"` would be asked for everything. The key list is
/// `workflows::email_summary_config::REQUIRED_KEYS`, the one `web` and
/// `workflows-service` refuse to boot without, so a key made mandatory there
/// fails this gate before it can crash-loop a staging roll (26.10.10-rc.1).
pub fn unsatisfied_summary_keys(deployment: &Deployment) -> Result<Vec<&'static str>> {
    use workflows::email_summary_config::{self as summary, DEPLOYMENT_ENV, REQUIRED_KEYS};

    let enabled = summary::enabled(&|key: &str| deployment.coordinates.get(key).cloned())
        .with_context(|| format!("{}: read the summary lane switch", deployment.name))?;
    if !enabled {
        return Ok(Vec::new());
    }
    let supplied = deployment.supplied_keys();
    Ok(REQUIRED_KEYS
        .iter()
        .copied()
        // `ops ship` renders the row's own name into both pods.
        .filter(|key| *key != DEPLOYMENT_ENV && !supplied.contains(*key))
        .collect())
}

/// Every boot requirement this row leaves unsatisfied, as the key names that
/// would satisfy each one.
fn unsatisfied_boot_requirements(deployment: &Deployment) -> Result<Vec<String>> {
    Ok(unsatisfied_requirements(deployment)?
        .iter()
        .map(describe)
        .chain(
            unsatisfied_summary_keys(deployment)?
                .into_iter()
                .map(ToString::to_string),
        )
        .collect())
}

/// Render one unsatisfied requirement as the key names that would satisfy it.
#[must_use]
pub fn describe(requirement: &Requirement) -> String {
    requirement
        .any_of
        .iter()
        .map(|alternative| alternative.join(" + "))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The Secret Manager objects a deployment must populate: every key the
/// shipped `SecretProviderClass` projects into the pod's Secret.
#[must_use]
pub fn projected_objects() -> BTreeSet<String> {
    ship::secret_provider_class_keys()
}

/// Where each projected object's value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Coordinate,
    Encrypted,
}

/// Why an object the `SecretProviderClass` references is legitimately absent
/// from one deployment's tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exemption {
    /// The object belongs to a deployment other than this one. The
    /// `SecretProviderClass` is one embedded manifest rendered for every
    /// deployment, so it references the engineering webhook receiver's
    /// credentials everywhere — while `store::deployment` says that receiver
    /// runs in exactly one project and no other deployment may hold them.
    /// Demanding them would push a singleton's HMAC secret into a production
    /// row that must not have it.
    ScopedElsewhere,
    /// The object is one alternative of a requirement this deployment already
    /// satisfies another way. `DOCUSIGN_ACCESS_TOKEN` is the case in hand: it
    /// is an alternative to the `DOCUSIGN_INTEGRATION_KEY` + `_USER_ID` +
    /// `_PRIVATE_KEY` JWT triple, so a deployment using JWT auth correctly has
    /// no access token and the manifest over-specifies by naming both.
    AlternativeSatisfied,
    /// The object belongs to an integration this deployment declines outright.
    /// A `Requirement` carrying a `trigger` is demanded only once that trigger
    /// key is supplied, so a deployment supplying none of the integration has
    /// nothing to satisfy — and must not be handed a placeholder to get past
    /// this check. `example-project` declines `DocuSign` (no `DOCUSIGN_BASE_URL`)
    /// and runs `StubSignatureProvider`, which `portal::signature` reaches only
    /// through genuine absence.
    Untriggered,
}

impl Exemption {
    const fn reason(self) -> &'static str {
        match self {
            Self::ScopedElsewhere => "scoped to another deployment",
            Self::AlternativeSatisfied => "requirement satisfied another way",
            Self::Untriggered => "integration not declared by this deployment",
        }
    }
}

/// Whether `object` is some requirement's trigger key that this deployment does
/// not supply.
///
/// A trigger is optional by construction — supplying it is what turns the
/// integration on — so it is never itself a requirement and never appears in an
/// `any_of`. Without this, the trigger would be the one object of a declined
/// integration that no rule could exempt.
fn is_undeclared_trigger(object: &str, supplied: &BTreeSet<String>) -> bool {
    if supplied.contains(object) {
        return false;
    }
    let is_trigger = store::deployment::WEB_REQUIREMENTS
        .iter()
        .any(|requirement| requirement.trigger == Some(object));
    if !is_trigger {
        return false;
    }
    // A projected trigger that an active requirement also demands is a gap.
    // Firm integrations use that shape: each coordinate triggers the other,
    // so supplying one and omitting the other cannot hide behind "undeclared".
    let demanded_by_active = store::deployment::WEB_REQUIREMENTS
        .iter()
        .any(|requirement| {
            requirement
                .trigger
                .is_some_and(|trigger| supplied.contains(trigger))
                && requirement
                    .any_of
                    .iter()
                    .any(|alternative| alternative.contains(&object))
        });
    !demanded_by_active
}

/// Whether this deployment may legitimately omit `object`.
///
/// Derived from `WEB_REQUIREMENTS` itself — its capability scoping, its
/// `trigger`, and its `any_of` alternatives — rather than a second
/// hand-maintained list, because a hand-maintained copy is the drift this whole
/// tree exists to remove. The three filters are exactly the ones
/// `store::deployment::applicable_web_requirements` applies when `web` enforces
/// the same table at boot; reading fewer of them here is what made a declined
/// integration look like a missing credential. An object no requirement
/// mentions at all is never exempt: a Drive folder id is projected everywhere on
/// purpose, and its absence is a real gap.
fn exemption(
    object: &str,
    capabilities: store::deployment::DeploymentCapabilities,
    supplied: &BTreeSet<String>,
) -> Option<Exemption> {
    if is_undeclared_trigger(object, supplied) {
        return Some(Exemption::Untriggered);
    }
    let mut verdict = None;
    for requirement in store::deployment::WEB_REQUIREMENTS {
        if !requirement
            .any_of
            .iter()
            .any(|alternative| alternative.contains(&object))
        {
            continue;
        }
        let scoped_elsewhere = !requirement.scope.applies(capabilities);
        let untriggered = requirement
            .trigger
            .is_some_and(|trigger| !supplied.contains(trigger));
        let satisfied_otherwise = requirement.any_of.iter().any(|alternative| {
            !alternative.contains(&object) && alternative.iter().all(|key| supplied.contains(*key))
        });
        let reason = if scoped_elsewhere {
            Exemption::ScopedElsewhere
        } else if untriggered {
            Exemption::Untriggered
        } else if satisfied_otherwise {
            Exemption::AlternativeSatisfied
        } else {
            // One requirement genuinely wants this object here, which settles
            // it however the others read.
            return None;
        };
        verdict = Some(reason);
    }
    verdict
}

/// The Secret Manager catalog objects this deployment legitimately does not
/// write — the set the rendered `SecretProviderClass` must not reference.
///
/// Read straight off [`plan`], so the rendered object list and the objects
/// `ops secrets apply` writes cannot drift: what the class references is exactly
/// what the deployment supplies. That symmetry is the whole property. An entry
/// the deployment will never write is not a harmless reference: it makes the
/// Secret Manager preflight fail before a metadata-only ship can reconcile.
///
/// # Errors
///
/// Propagates [`plan`]'s failure when an object is supplied by neither file and
/// no rule exempts it: a real gap in the deployment's tree, not something to
/// quietly omit from the catalog.
pub fn skipped_projected_objects(deployment: &Deployment) -> Result<BTreeSet<String>> {
    let (_, skipped) = plan(deployment)?;
    Ok(skipped.into_keys().collect())
}

/// `navigator ops deployments --deployments-dir <dir>` — run every
/// tree-level gate against whatever tree it is pointed at.
///
/// The workspace suite asserts these against the synthetic tree in
/// `cli/tests/fixtures/deployment-tree/` and against this repository's own
/// `deployments/`, and `ci.yml`'s `gate` job runs this command over the real
/// rows on every pull request.
///
/// Four things, and each has caught something:
///
/// 1. Every row loads — the config parses, names its project, and its
///    `secrets.enc.yaml` is per-value encrypted rather than a plaintext file
///    somebody forgot to run `sops` over.
/// 2. No stray decrypted file sits beside it. A `secrets.yaml` would not be
///    loaded at all, so loading is not enough; the directory is walked.
/// 3. Every `.sops.yaml` rule agrees with the `kms_key` its row declares, and
///    that key lives in that row's own project. Disagreement means one
///    deployment's key material is encrypted against another's — the isolation
///    failing silently, because both still decrypt for whoever holds both.
/// 4. Every boot requirement that applies to a row is satisfied by that row's
///    own files, and every object the `SecretProviderClass` projects is
///    supplied or explicitly skipped.
///
/// Names only. Nothing here decrypts, so it runs in CI with no KMS grant, no
/// credential, and no network — the same asymmetry [`Deployment::load`] draws.
pub fn check(root: &Path) -> Result<()> {
    let names = names(root)?;
    if names.is_empty() {
        bail!(
            "{}/{TREE} describes no deployment. An empty tree passes every check below \
             vacuously, which is indistinguishable from a healthy one",
            root.display()
        );
    }

    let sops_rules = fs::read_to_string(root.join(SOPS_CONFIG))
        .with_context(|| format!("read {}/{SOPS_CONFIG}", root.display()))?;

    let mut checked = 0_usize;
    for name in &names {
        let deployment = Deployment::load(root, name)?;
        no_stray_plaintext(root, name)?;

        if !sops_rules.contains(&deployment.kms_key) {
            bail!(
                "no {SOPS_CONFIG} creation rule names {name}'s key {}, so the next `sops` write \
                 to that row encrypts against some other rule's key",
                deployment.kms_key
            );
        }
        if !deployment.kms_key.contains(deployment.project_id()) {
            bail!(
                "{name}'s key must live in its own project ({}), or the per-deployment isolation \
                 is nominal: {}",
                deployment.project_id(),
                deployment.kms_key
            );
        }

        // An unprovisioned row describes resources nobody has created yet, so
        // its coordinates are deliberately blank and it carries no encrypted
        // file. Demanding completeness of it would only force placeholder
        // values into the tree that read as real.
        if !deployment.provisioned {
            eprintln!("{name}: declared unprovisioned, completeness not checked");
            continue;
        }

        let unsatisfied = unsatisfied_boot_requirements(&deployment)?;
        if !unsatisfied.is_empty() {
            bail!(
                "{name} does not satisfy {} boot requirement(s) that apply to it: {}",
                unsatisfied.len(),
                unsatisfied.join(", ")
            );
        }

        let (plan, skipped) = plan(&deployment)?;
        eprintln!(
            "{name}: {} object(s) supplied, {} skipped{}",
            plan.len(),
            skipped.len(),
            if skipped.is_empty() {
                String::new()
            } else {
                format!(
                    " ({})",
                    skipped
                        .iter()
                        .map(|(object, reason)| format!("{object}: {}", reason.reason()))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
        checked += 1;
    }

    eprintln!(
        "checked {} deployment(s) in {}/{TREE}, {checked} provisioned",
        names.len(),
        root.display()
    );
    Ok(())
}

/// A decrypted working copy must never sit beside the encrypted one.
///
/// `Deployment::load` refuses a plaintext value inside `secrets.enc.yaml`, but
/// a stray `secrets.yaml` is a different file and would not be loaded at all.
/// The directory is walked so the accident is caught by its filename.
fn no_stray_plaintext(root: &Path, name: &str) -> Result<()> {
    let dir = root.join(TREE).join(name);
    for entry in
        fs::read_dir(&dir).with_context(|| format!("read the tree at {}", dir.display()))?
    {
        let path = entry.context("read a deployment directory entry")?.path();
        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if file_name == CONFIG_FILE
            || file_name == SECRETS_FILE
            || file_name == ServiceSecrets::Postgres.file_name()
            || file_name == ServiceSecrets::Forgejo.file_name()
        {
            continue;
        }
        bail!(
            "{TREE}/{name}/{file_name} is neither the plaintext coordinates nor the encrypted key \
             material. A decrypted file must never be written here: committing one cannot be \
             undone, only rotated at the provider"
        );
    }
    Ok(())
}

/// Resolve every projected object to the file that supplies it, or fail
/// naming the objects nothing supplies. Names only — no value is read here.
///
/// Returns the plan and the objects deliberately skipped, so the caller can
/// report a skip by name. A skip is never silent: an operator reading the
/// output must be able to see that this deployment writes fewer objects than
/// the manifest references, and why.
fn plan(
    deployment: &Deployment,
) -> Result<(BTreeMap<String, Source>, BTreeMap<String, Exemption>)> {
    let capabilities = store::deployment::DeploymentCapabilities::from_lookup(&|key| {
        deployment.coordinates.get(key).cloned()
    })
    .with_context(|| format!("parse capability flags in deployments/{}", deployment.name))?;
    let supplied = deployment.supplied_keys();
    let mut plan = BTreeMap::new();
    let mut skipped = BTreeMap::new();
    let mut missing = Vec::new();
    for object in projected_objects() {
        if deployment.encrypted_keys.contains(&object) {
            plan.insert(object, Source::Encrypted);
        } else if deployment.coordinates.contains_key(&object) {
            plan.insert(object, Source::Coordinate);
        } else if let Some(reason) = exemption(&object, capabilities, &supplied) {
            skipped.insert(object, reason);
        } else {
            missing.push(object);
        }
    }
    if !missing.is_empty() {
        bail!(
            "{} object(s) the SecretProviderClass projects are in neither {CONFIG_FILE} nor \
             {SECRETS_FILE} for {}: {}. Secret Manager was not changed.",
            missing.len(),
            deployment.name,
            missing.join(", ")
        );
    }
    Ok((plan, skipped))
}

/// `navigator ops secrets apply --deployment <name>` — decrypt this
/// deployment's key material and write `versions/latest` for every object the
/// `SecretProviderClass` projects, in that deployment's own project.
///
/// `dry_run` returns after the names-only plan. It does not shell out to
/// `sops`, so it needs no KMS decrypt permission and produces no plaintext to
/// leak — the point of a dry run is to check the shape, and the shape is the
/// names.
pub fn apply(root: &Path, name: &str, dry_run: bool) -> Result<()> {
    let deployment = Deployment::load(root, name)?;

    // Boot invariants before Secret Manager objects: a deployment missing a
    // requirement that applies to it will crash-loop on the values this
    // command is about to write, and finding that out here costs nothing.
    let unsatisfied = unsatisfied_boot_requirements(&deployment)?;
    if !unsatisfied.is_empty() {
        bail!(
            "{name} does not satisfy {} boot requirement(s) that apply to it: {}. Secret Manager \
             was not changed.",
            unsatisfied.len(),
            unsatisfied.join(", ")
        );
    }

    let (plan, skipped) = plan(&deployment)?;
    let project_id = deployment.project_id().to_owned();
    let ship_config = ship::ShipConfig::from_deployment(&deployment)?;

    eprintln!(
        "secrets apply: deployment={name} project={project_id} object(s)={}",
        plan.len()
    );
    eprintln!(
        "from {CONFIG_FILE}: {}",
        join_names(&plan, Source::Coordinate)
    );
    eprintln!(
        "from {SECRETS_FILE}: {}",
        join_names(&plan, Source::Encrypted)
    );
    if !skipped.is_empty() {
        eprintln!(
            "skipped {} object(s) this deployment may legitimately omit: {}",
            skipped.len(),
            skipped
                .iter()
                .map(|(object, reason)| format!("{object} ({})", reason.reason()))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if dry_run {
        eprintln!("DRY-RUN: nothing was decrypted and Secret Manager was not changed");
        return Ok(());
    }

    let decrypted = decrypt(&root.join(TREE).join(name).join(SECRETS_FILE))?;
    let payloads = resolve_payloads(&deployment, &plan, &decrypted)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(async {
        let token = gcp::auth::adc_token_provider().await?;
        // Never a dry-run client: its recorder serializes the request body,
        // which is where the payload rides.
        let client = gcp::client::GcpClient::new(token);
        for (object, value) in &payloads {
            gcp::secret_manager::ensure_secret(&client, &project_id, object).await?;
            gcp::secret_manager::add_version(&client, &project_id, object, value.as_bytes())
                .await?;
            eprintln!("wrote a new version of {object}");
        }
        eprintln!(
            "applied {} object(s) to projects/{project_id}/secrets",
            payloads.len()
        );
        Ok::<(), anyhow::Error>(())
    })?;
    // The row's own cluster, never a `KUBECONFIG` from the shell or `.devx/env`.
    let _credentials = ship::ShipCredentials::cluster(&ship_config)?;
    apply_web_secret(&ship_config, &payloads)
}

/// Write the Secret that the rendered `web` Deployment reads through `envFrom`.
///
/// The values are already present in memory after the SOPS decrypt and Secret
/// Manager version write. They travel only over `kubectl` stdin; no value is
/// included in an argument, log line, or error context.
fn apply_web_secret(config: &ship::ShipConfig, payloads: &BTreeMap<String, String>) -> Result<()> {
    write_web_secret(config, payloads, |manifest| {
        let mut child = Command::new("kubectl")
            .args([
                "--context",
                config.context.as_str(),
                "--namespace",
                config.namespace.as_str(),
            ])
            .args(WEB_SECRET_APPLY_ARGS)
            .stdin(Stdio::piped())
            .spawn()
            .context("start kubectl apply for deployment web Secret")?;
        child
            .stdin
            .take()
            .context("kubectl stdin unavailable for deployment web Secret")?
            .write_all(manifest.as_bytes())
            .context("write deployment web Secret manifest to kubectl")?;
        let status = child
            .wait()
            .context("wait for deployment web Secret apply")?;
        if !status.success() {
            bail!("kubectl apply for deployment web Secret failed ({status})");
        }
        Ok(())
    })?;
    eprintln!("applied Kubernetes Secret for deployment web");
    Ok(())
}

/// Build and hand the deployment's `envFrom` Secret to the caller that writes
/// it. Splitting the transport from the manifest keeps the value-bearing write
/// testable without routing a credential through a recorder.
fn write_web_secret<F>(
    config: &ship::ShipConfig,
    payloads: &BTreeMap<String, String>,
    write: F,
) -> Result<()>
where
    F: FnOnce(&str) -> Result<()>,
{
    let manifest = serde_yaml::to_string(&serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": config.secret_name,
            "namespace": config.namespace,
        },
        "type": "Opaque",
        "stringData": payloads,
    }))
    .context("serialize deployment web Secret manifest")?;
    write(&manifest)
}

#[derive(Clone, PartialEq, Eq)]
struct KubernetesSecretPayload {
    name: &'static str,
    secret_type: &'static str,
    data: BTreeMap<String, String>,
}

/// `data` holds decrypted values, so a `{:?}` (an `assert_eq!` failure, a
/// `dbg!`, an error context) shows only the key names.
impl std::fmt::Debug for KubernetesSecretPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KubernetesSecretPayload")
            .field("name", &self.name)
            .field("secret_type", &self.secret_type)
            .field("keys", &self.data.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// Validate the selected service's inputs using only key names and plaintext
/// coordinates. The two services have distinct encrypted files so this check
/// cannot accidentally make one service's apply depend on decrypting another.
fn service_plan(deployment: &Deployment, service: ServiceSecrets) -> Result<BTreeSet<String>> {
    if !deployment.provisioned {
        bail!(
            "{} is declared unprovisioned; service secrets cannot be applied",
            deployment.name
        );
    }
    let supplied = service.encrypted_keys(deployment);
    let allowed: BTreeSet<String> = service.allowed_keys().map(str::to_owned).collect();
    let mut expected: BTreeSet<String> =
        service.keys().iter().map(|key| (*key).to_owned()).collect();
    // Any key of an optional set makes the whole set required, so the
    // missing-key check below reports a partial set.
    for set in service.optional_key_sets() {
        if set.iter().any(|key| supplied.contains(*key)) {
            expected.extend(set.iter().map(|key| (*key).to_owned()));
        }
    }
    let unexpected: Vec<_> = supplied.difference(&allowed).cloned().collect();
    if !unexpected.is_empty() {
        bail!(
            "deployments/{}/{} contains key(s) not owned by the {} service: {}",
            deployment.name,
            service.file_name(),
            service.name(),
            unexpected.join(", ")
        );
    }
    let missing: Vec<_> = expected.difference(supplied).cloned().collect();
    if !missing.is_empty() {
        bail!(
            "{} service for {} is missing encrypted key(s) in deployments/{}/{}: {}",
            service.name(),
            deployment.name,
            deployment.name,
            service.file_name(),
            missing.join(", ")
        );
    }
    if service == ServiceSecrets::Forgejo {
        forgejo_signing_identity(deployment)?;
        let root_url = deployment
            .coordinates
            .get("NAVIGATOR_FORGEJO_ROOT_URL")
            .map(String::as_str)
            .unwrap_or_default();
        if !(root_url.starts_with("https://")
            && root_url.ends_with('/')
            && !root_url.contains(['\n', '\r', ' ']))
        {
            bail!(
                "{} must set NAVIGATOR_FORGEJO_ROOT_URL to an HTTPS URL ending in `/` before Forgejo secrets can be applied",
                deployment.name
            );
        }
        if expected.contains(FORGEJO_OIDC_KEYS[0]) {
            let client_id = deployment
                .coordinates
                .get(FORGEJO_OIDC_CLIENT_ID)
                .map(String::as_str)
                .unwrap_or_default();
            if client_id.trim().is_empty() || client_id.contains(['\n', '\r']) {
                bail!(
                    "{} must set {FORGEJO_OIDC_CLIENT_ID} to Forgejo's OIDC client id before its client secret can be applied",
                    deployment.name
                );
            }
        }
    }
    Ok(expected)
}

fn service_payloads(
    service: ServiceSecrets,
    keys: &BTreeSet<String>,
    decrypted: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let decrypted_keys: BTreeSet<String> = decrypted.keys().cloned().collect();
    ensure!(
        &decrypted_keys == keys,
        "decrypted service file key names differ from the validated key set"
    );
    let mut payloads = BTreeMap::new();
    for key in keys {
        let value = decrypted
            .get(key)
            .filter(|value| !value.is_empty())
            .with_context(|| format!("{key} is missing or empty in the encrypted service file"))?;
        payloads.insert(key.clone(), value.clone());
    }
    ensure!(
        payloads
            .keys()
            .all(|key| service.allowed_keys().any(|allowed| allowed == key)),
        "service secret plan contains a key outside the selected service"
    );
    Ok(payloads)
}

/// One Secret per PostgreSQL login present in `payloads` — the client set and
/// the optional DBOS login — each with its own `password` and the shared
/// `ca.crt`.
fn postgres_login_secrets(
    payloads: &BTreeMap<String, String>,
) -> Result<Vec<KubernetesSecretPayload>> {
    let mut logins: Vec<(&'static str, &'static str)> = Vec::new();
    if payloads.contains_key(POSTGRES_CLIENT_KEYS[0]) {
        logins.extend_from_slice(POSTGRES_CLIENT_SECRETS);
    }
    if payloads.contains_key(POSTGRES_DBOS_KEYS[0]) {
        logins.push((POSTGRES_DBOS_SECRET, POSTGRES_DBOS_KEYS[0]));
    }
    if logins.is_empty() {
        return Ok(Vec::new());
    }
    let ca = required_payload(payloads, "NAVIGATOR_POSTGRES_CA_CERT")?;
    logins
        .into_iter()
        .map(|(name, password)| {
            Ok(KubernetesSecretPayload {
                name,
                secret_type: "Opaque",
                data: BTreeMap::from([
                    ("password".to_owned(), required_payload(payloads, password)?),
                    ("ca.crt".to_owned(), ca.clone()),
                ]),
            })
        })
        .collect()
}

/// Build the selected service's Kubernetes file Secrets. No service payload
/// is returned in a form that can be added to the Navigator app's env Secret.
fn build_service_secrets(
    service: ServiceSecrets,
    deployment: &Deployment,
    namespace: &str,
    payloads: &BTreeMap<String, String>,
) -> Result<Vec<KubernetesSecretPayload>> {
    match service {
        ServiceSecrets::Postgres => {
            let mut secrets = vec![
                KubernetesSecretPayload {
                    name: "navigator-postgres-auth",
                    secret_type: "Opaque",
                    data: BTreeMap::from([(
                        "password".to_owned(),
                        required_payload(payloads, "NAVIGATOR_POSTGRES_PASSWORD")?,
                    )]),
                },
                KubernetesSecretPayload {
                    name: "navigator-postgres-tls",
                    secret_type: "kubernetes.io/tls",
                    data: BTreeMap::from([
                        (
                            "tls.crt".to_owned(),
                            required_payload(payloads, "NAVIGATOR_POSTGRES_TLS_CERTIFICATE")?,
                        ),
                        (
                            "tls.key".to_owned(),
                            required_payload(payloads, "NAVIGATOR_POSTGRES_TLS_PRIVATE_KEY")?,
                        ),
                    ]),
                },
            ];
            secrets.extend(postgres_login_secrets(payloads)?);
            Ok(secrets)
        }
        ServiceSecrets::Forgejo => {
            let root_url = deployment
                .coordinates
                .get("NAVIGATOR_FORGEJO_ROOT_URL")
                .expect("service_plan validates the Forgejo URL");
            let app_ini =
                render_service_forgejo_app_ini(deployment, root_url, namespace, payloads)?;
            let mut secrets = vec![
                KubernetesSecretPayload {
                    name: "navigator-forgejo-config",
                    secret_type: "Opaque",
                    data: BTreeMap::from([
                        ("app.ini".to_owned(), app_ini),
                        (
                            "oauth2_jwt_secret".to_owned(),
                            required_payload(payloads, "NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET")?,
                        ),
                    ]),
                },
                KubernetesSecretPayload {
                    name: "navigator-forgejo-postgres-ca",
                    secret_type: "Opaque",
                    data: BTreeMap::from([(
                        "ca.crt".to_owned(),
                        required_payload(payloads, "NAVIGATOR_FORGEJO_POSTGRES_CA_CERT")?,
                    )]),
                },
                // Read only by the `navigator-forgejo-database` Job, which
                // sets the `forgejo` login's password from it.
                KubernetesSecretPayload {
                    name: "navigator-forgejo-database",
                    secret_type: "Opaque",
                    data: BTreeMap::from([(
                        "password".to_owned(),
                        required_payload(payloads, "NAVIGATOR_FORGEJO_DATABASE_PASSWORD")?,
                    )]),
                },
            ];
            if payloads.contains_key(FORGEJO_OIDC_KEYS[0]) {
                let client_id = deployment
                    .coordinates
                    .get(FORGEJO_OIDC_CLIENT_ID)
                    .cloned()
                    .context("service_plan validates the Forgejo OIDC client id")?;
                secrets.push(KubernetesSecretPayload {
                    name: "navigator-forgejo-oidc",
                    secret_type: "Opaque",
                    data: BTreeMap::from([
                        ("client_id".to_owned(), client_id),
                        (
                            "client_secret".to_owned(),
                            required_payload(payloads, FORGEJO_OIDC_KEYS[0])?,
                        ),
                    ]),
                });
            }
            if forgejo_signing_identity(deployment)?.is_some() {
                secrets.push(forgejo_signing_secret(payloads)?);
            }
            Ok(secrets)
        }
    }
}

fn forgejo_signing_secret(payloads: &BTreeMap<String, String>) -> Result<KubernetesSecretPayload> {
    Ok(KubernetesSecretPayload {
        name: "navigator-forgejo-signing",
        secret_type: "Opaque",
        data: BTreeMap::from([
            (
                "signing_key".to_owned(),
                required_payload(payloads, FORGEJO_SIGNING_KEYS[0])?,
            ),
            (
                "signing_key.pub".to_owned(),
                required_payload(payloads, FORGEJO_SIGNING_KEYS[1])?,
            ),
        ]),
    })
}

fn required_payload(payloads: &BTreeMap<String, String>, key: &str) -> Result<String> {
    payloads
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .with_context(|| format!("{key} is missing from the selected service payload"))
}

const FORGEJO_APP_INI_TEMPLATE: &str = include_str!("../../../k8s/forgejo/app.ini.example");

fn render_service_forgejo_app_ini(
    deployment: &Deployment,
    root_url: &str,
    namespace: &str,
    payloads: &BTreeMap<String, String>,
) -> Result<String> {
    let signing = forgejo_signing_identity(deployment)?;
    let mut rendered = render_forgejo_app_ini(root_url, namespace, payloads)?;
    let disabled = "[actions]\nENABLED = false\n";
    ensure!(
        rendered.matches(disabled).count() == 1,
        "Forgejo Actions default must appear exactly once"
    );
    if forgejo_actions_enabled(deployment)? {
        rendered = rendered.replacen(disabled, "[actions]\nENABLED = true\n", 1);
    }
    if let Some((name, email)) = signing {
        use std::fmt::Write as _;
        write!(rendered,
            "[repository.signing]\nFORMAT = ssh\nSIGNING_KEY = /etc/forgejo-signing/signing_key.pub\n\
             SIGNING_NAME = {name}\nSIGNING_EMAIL = {email}\n\
             INITIAL_COMMIT = always\nCRUD_ACTIONS = always\nMERGES = always\n"
        )?;
    }
    Ok(rendered)
}

pub(super) fn render_forgejo_app_ini(
    root_url: &str,
    namespace: &str,
    payloads: &BTreeMap<String, String>,
) -> Result<String> {
    let database_host = format!("navigator-postgres.{namespace}.svc.cluster.local:5432");
    let database_password = required_payload(payloads, "NAVIGATOR_FORGEJO_DATABASE_PASSWORD")?;
    let secret_key = required_payload(payloads, "NAVIGATOR_FORGEJO_SECRET_KEY")?;
    let internal_token = required_payload(payloads, "NAVIGATOR_FORGEJO_INTERNAL_TOKEN")?;
    let oauth2_jwt_secret = required_payload(payloads, "NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET")?;
    let decoded_jwt_secret = URL_SAFE_NO_PAD
        .decode(oauth2_jwt_secret.as_bytes())
        .context("NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET must be base64-encoded")?;
    ensure!(
        decoded_jwt_secret.len() == 32,
        "NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET must decode to exactly 32 bytes"
    );
    let substitutions = [
        ("https://forge.example.invalid/", root_url),
        ("navigator-postgres:5432", database_host.as_str()),
        (
            "__NAVIGATOR_FORGEJO_DATABASE_PASSWORD__",
            database_password.as_str(),
        ),
        ("__NAVIGATOR_FORGEJO_SECRET_KEY__", secret_key.as_str()),
        (
            "__NAVIGATOR_FORGEJO_INTERNAL_TOKEN__",
            internal_token.as_str(),
        ),
    ];
    let mut rendered = FORGEJO_APP_INI_TEMPLATE.to_owned();
    for (placeholder, value) in substitutions {
        if value.contains(['\n', '\r']) {
            bail!("a Forgejo app.ini input for {placeholder} must be a single line");
        }
        let occurrences = rendered.matches(placeholder).count();
        ensure!(
            occurrences == 1,
            "Forgejo app.ini template must contain {placeholder} exactly once"
        );
        rendered = rendered.replacen(placeholder, value, 1);
    }
    ensure!(
        !rendered.contains("__NAVIGATOR_") && !rendered.contains("forge.example.invalid"),
        "Forgejo app.ini template has an unresolved deployment placeholder"
    );
    Ok(rendered)
}

fn write_service_secrets<F>(
    namespace: &str,
    secrets: &[KubernetesSecretPayload],
    write: F,
) -> Result<()>
where
    F: FnOnce(&str) -> Result<()>,
{
    let mut documents = Vec::new();
    for secret in secrets {
        let manifest = serde_yaml::to_string(&serde_json::json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": {"name": secret.name, "namespace": namespace},
            "type": secret.secret_type,
            "stringData": secret.data,
        }))
        .context("serialize service Kubernetes Secret manifest")?;
        documents.push(manifest);
    }
    write(&documents.join("---\n"))
}

/// Apply one service's independent SOPS file to Secret Manager and to only its
/// own mounted-file Kubernetes Secrets. Dry-run remains names-only and never
/// decrypts, authenticates, or invokes kubectl.
pub fn apply_service(
    root: &Path,
    name: &str,
    service: ServiceSecrets,
    dry_run: bool,
) -> Result<()> {
    let deployment = Deployment::load(root, name)?;
    let keys = service_plan(&deployment, service)?;
    let ship_config = ship::ShipConfig::from_deployment(&deployment)?;
    let file = service.file_name();
    eprintln!(
        "secrets apply: deployment={name} service={} project={} object(s)={}",
        service.name(),
        deployment.project_id(),
        keys.len()
    );
    eprintln!(
        "from deployments/{name}/{file}: {}",
        keys.iter().cloned().collect::<Vec<_>>().join(", ")
    );
    eprintln!(
        "Kubernetes Secret target(s): {}",
        service.targets(&keys).join(", ")
    );
    if dry_run {
        eprintln!("DRY-RUN: nothing was decrypted; Secret Manager and Kubernetes were not changed");
        return Ok(());
    }

    let encrypted_path = root.join(TREE).join(name).join(file);
    let decrypted = decrypt(&encrypted_path)?;
    let payloads = service_payloads(service, &keys, &decrypted)?;
    let kubernetes_secrets =
        build_service_secrets(service, &deployment, &ship_config.namespace, &payloads)?;
    let project_id = deployment.project_id().to_owned();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(async {
        let token = gcp::auth::adc_token_provider().await?;
        let client = gcp::client::GcpClient::new(token);
        for (object, value) in &payloads {
            gcp::secret_manager::ensure_secret(&client, &project_id, object).await?;
            gcp::secret_manager::add_version(&client, &project_id, object, value.as_bytes())
                .await?;
            eprintln!("wrote a new version of {object}");
        }
        Ok::<(), anyhow::Error>(())
    })?;
    write_service_secrets(&ship_config.namespace, &kubernetes_secrets, |manifest| {
        kubectl_apply_service_secrets(&ship_config, manifest)
    })?;
    eprintln!("applied Kubernetes Secret(s) for {}", service.name());
    Ok(())
}

/// Hand service Secret manifests to `kubectl` over stdin, never argv.
fn kubectl_apply_service_secrets(ship_config: &ship::ShipConfig, manifest: &str) -> Result<()> {
    {
        let mut child = Command::new("kubectl")
            .args([
                "--context",
                ship_config.context.as_str(),
                "--namespace",
                ship_config.namespace.as_str(),
            ])
            .args(SERVICE_SECRET_APPLY_ARGS)
            .stdin(Stdio::piped())
            .spawn()
            .context("start kubectl apply for service Secrets")?;
        child
            .stdin
            .take()
            .context("kubectl stdin unavailable for service Secrets")?
            .write_all(manifest.as_bytes())
            .context("write service Secret manifests to kubectl")?;
        let status = child.wait().context("wait for service Secret apply")?;
        if !status.success() {
            bail!("kubectl apply for service Secrets failed ({status})");
        }
        Ok(())
    }
}

/// Read one secret value from Secret Manager `versions/latest`. The error
/// names the key and the command that supplies it, never a value.
async fn record_value(client: &gcp::client::GcpClient, project: &str, key: &str) -> Result<String> {
    let bytes = gcp::secret_manager::access_version(client, project, key, "latest")
        .await?
        .with_context(|| {
            format!("{key} has no value in Secret Manager; supply it with `ops secrets set`")
        })?;
    String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("{key} in Secret Manager is not UTF-8"))
}

/// Resolve the web plan for a record-backed row: secret objects are read from
/// Secret Manager, and coordinate objects are written there so the CSI driver
/// projects the same set the tree flow writes.
async fn record_payloads(
    client: &gcp::client::GcpClient,
    deployment: &Deployment,
    plan: &BTreeMap<String, Source>,
) -> Result<BTreeMap<String, String>> {
    let project = deployment.project_id();
    let mut payloads = BTreeMap::new();
    for (object, source) in plan {
        let value = match source {
            Source::Encrypted => record_value(client, project, object).await?,
            Source::Coordinate => {
                let value = deployment.coordinates[object].clone();
                gcp::secret_manager::ensure_secret(client, project, object).await?;
                gcp::secret_manager::add_version(client, project, object, value.as_bytes()).await?;
                eprintln!("wrote a new version of {object}");
                value
            }
        };
        ensure!(!value.is_empty(), "{object} resolved to an empty value");
        payloads.insert(object.clone(), value);
    }
    Ok(payloads)
}

/// `ops secrets apply --project-id`: project the Kubernetes Secret(s) from the
/// published record and the values already in Secret Manager. Nothing is
/// decrypted; values come from `ops secrets set`, not SOPS.
pub(super) fn apply_from_record(
    deployment: &Deployment,
    service: Option<ServiceSecrets>,
    dry_run: bool,
) -> Result<()> {
    let name = deployment.name.as_str();
    let project_id = deployment.project_id().to_owned();
    let ship_config = ship::ShipConfig::from_deployment(deployment)?;
    let runtime = || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime")
    };
    if let Some(service) = service {
        let keys = service_plan(deployment, service)?;
        eprintln!(
            "secrets apply: deployment={name} service={} project={project_id} object(s)={} \
             source=Secret Manager",
            service.name(),
            keys.len()
        );
        eprintln!(
            "Kubernetes Secret target(s): {}",
            service.targets(&keys).join(", ")
        );
        if dry_run {
            eprintln!("DRY-RUN: nothing was read; Kubernetes was not changed");
            return Ok(());
        }
        let values = runtime()?.block_on(async {
            let client = gcp::client::GcpClient::new(gcp::auth::adc_token_provider().await?);
            let mut values = BTreeMap::new();
            for key in &keys {
                values.insert(key.clone(), record_value(&client, &project_id, key).await?);
            }
            Ok::<_, anyhow::Error>(values)
        })?;
        let payloads = service_payloads(service, &keys, &values)?;
        let secrets =
            build_service_secrets(service, deployment, &ship_config.namespace, &payloads)?;
        let _credentials = ship::ShipCredentials::cluster(&ship_config)?;
        write_service_secrets(&ship_config.namespace, &secrets, |manifest| {
            kubectl_apply_service_secrets(&ship_config, manifest)
        })?;
        eprintln!("applied Kubernetes Secret(s) for {}", service.name());
        return Ok(());
    }

    let unsatisfied = unsatisfied_boot_requirements(deployment)?;
    if !unsatisfied.is_empty() {
        bail!(
            "{name} does not satisfy {} boot requirement(s) that apply to it: {}. Secret Manager \
             was not changed.",
            unsatisfied.len(),
            unsatisfied.join(", ")
        );
    }
    let (plan, _skipped) = plan(deployment)?;
    eprintln!(
        "secrets apply: deployment={name} project={project_id} object(s)={} source=Secret Manager",
        plan.len()
    );
    eprintln!("from the record: {}", join_names(&plan, Source::Coordinate));
    eprintln!(
        "from Secret Manager: {}",
        join_names(&plan, Source::Encrypted)
    );
    if dry_run {
        eprintln!("DRY-RUN: nothing was read and Secret Manager was not changed");
        return Ok(());
    }
    let payloads = runtime()?.block_on(async {
        let client = gcp::client::GcpClient::new(gcp::auth::adc_token_provider().await?);
        record_payloads(&client, deployment, &plan).await
    })?;
    let _credentials = ship::ShipCredentials::cluster(&ship_config)?;
    apply_web_secret(&ship_config, &payloads)
}

fn join_names(plan: &BTreeMap<String, Source>, source: Source) -> String {
    plan.iter()
        .filter(|(_, entry)| **entry == source)
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Decrypt one deployment's key material through `sops`.
///
/// The plaintext lands in this process's memory and nowhere else: `sops`
/// writes it to a pipe, not to disk, and the values never reach `argv`, a log
/// line, or an error message. A failure surfaces `sops`'s own stderr, which
/// describes the KMS call rather than the file's contents.
fn decrypt(path: &Path) -> Result<BTreeMap<String, String>> {
    let output = Command::new("sops")
        .arg("--decrypt")
        .arg("--output-type")
        .arg("json")
        .arg(path)
        .output()
        .with_context(|| {
            format!(
                "run `sops --decrypt {}`. Install sops (`brew install sops`) and authenticate \
                 with a principal holding cloudkms.cryptoKeyDecrypter on this deployment's key.",
                path.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "sops could not decrypt {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout)
        // No `with_context` carrying the body: a parse error's payload is the
        // decrypted document.
        .map_err(|error| anyhow::anyhow!("parse the decrypted document: {error}"))
}

/// Resolve every planned object to its plaintext value — from the decrypted
/// SOPS document or, for a `Source::Coordinate` object, straight from
/// `config.toml`. Shared by [`apply`] (which writes these values) and
/// [`check_secrets`] (which only ever compares them).
fn resolve_payloads(
    deployment: &Deployment,
    plan: &BTreeMap<String, Source>,
    decrypted: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut payloads = BTreeMap::new();
    for (object, source) in plan {
        let value = match source {
            Source::Encrypted => decrypted.get(object).cloned(),
            Source::Coordinate => deployment.coordinates.get(object).cloned(),
        };
        let value = value
            .filter(|value| !value.is_empty())
            .with_context(|| format!("{object} resolved to an empty value"))?;
        payloads.insert(object.clone(), value);
    }
    Ok(payloads)
}

/// One object's drift status against one comparison target. The four values
/// named by ENG-887's acceptance criteria — never anything richer, so a
/// value or a digest has no path into this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Match,
    Differs,
    MissingInSecretManager,
    MissingInK8sSecret,
}

impl CheckStatus {
    fn label(self) -> &'static str {
        match self {
            CheckStatus::Match => "match",
            CheckStatus::Differs => "differs",
            CheckStatus::MissingInSecretManager => "missing in Secret Manager",
            CheckStatus::MissingInK8sSecret => "missing in K8s Secret",
        }
    }
}

/// Constant-time byte comparison. A length mismatch is not itself treated as
/// sensitive (an object's value length is not secret), but every byte
/// position that both sides share is still compared without an early return.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// `expected` is the freshly decrypted SOPS value; `actual` is what the same
/// object currently resolves to at the comparison target, or `None` when the
/// target has nothing under that name.
fn classify(expected: &[u8], actual: Option<&[u8]>, missing: CheckStatus) -> CheckStatus {
    match actual {
        None => missing,
        Some(actual) if constant_time_eq(expected, actual) => CheckStatus::Match,
        Some(_) => CheckStatus::Differs,
    }
}

/// Compare every resolved SOPS value against Secret Manager's `versions/latest`
/// and against the deployment's live Kubernetes Secret. Never returns, logs,
/// or prints a value — only the pair of [`CheckStatus`] verdicts per object.
async fn check_payloads(
    client: &gcp::client::GcpClient,
    project_id: &str,
    payloads: &BTreeMap<String, String>,
    k8s_values: &BTreeMap<String, Option<Vec<u8>>>,
) -> Result<BTreeMap<String, (CheckStatus, CheckStatus)>> {
    let mut statuses = BTreeMap::new();
    for (object, value) in payloads {
        let sm_value =
            gcp::secret_manager::access_version(client, project_id, object, "latest").await?;
        let sm_status = classify(
            value.as_bytes(),
            sm_value.as_deref(),
            CheckStatus::MissingInSecretManager,
        );
        let k8s_value = k8s_values.get(object).and_then(|value| value.as_deref());
        let k8s_status = classify(value.as_bytes(), k8s_value, CheckStatus::MissingInK8sSecret);
        statuses.insert(object.clone(), (sm_status, k8s_status));
    }
    Ok(statuses)
}

/// Render a `--check` report: one line per object per comparison target,
/// name and status word only. This is the only place `check` writes to
/// stdout, so a test can assert directly on this string that no value or
/// digest ever appears in it.
fn render_report(statuses: &BTreeMap<String, (CheckStatus, CheckStatus)>) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (object, (secret_manager, k8s)) in statuses {
        let _ = writeln!(out, "{object} (Secret Manager): {}", secret_manager.label());
        let _ = writeln!(out, "{object} (K8s Secret): {}", k8s.label());
    }
    out
}

/// The deployment's live `web` Kubernetes Secret, as raw `kubectl get -o
/// json` output, or `None` when the Secret does not exist yet (a deployment
/// `apply` has never reached). The shell-out itself is not covered by a
/// test — [`parse_k8s_secret_values`], which does the comparison-relevant
/// parsing, is.
fn fetch_k8s_secret_json(config: &ship::ShipConfig) -> Result<Option<String>> {
    let output = Command::new("kubectl")
        .args([
            "--context",
            config.context.as_str(),
            "--namespace",
            config.namespace.as_str(),
            "get",
            "secret",
            config.secret_name.as_str(),
            "-o",
            "json",
        ])
        .output()
        .context("read deployment web Kubernetes Secret")?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8(output.stdout).context("Kubernetes Secret was not UTF-8")?,
    ))
}

/// Decode the `.data` map of a `kubectl get secret -o json` payload for each
/// of `keys`. `json` is `None` when the Secret itself does not exist, which
/// makes every key `None` (missing), not a parse failure.
fn parse_k8s_secret_values<'a>(
    json: Option<&str>,
    keys: impl Iterator<Item = &'a String>,
) -> Result<BTreeMap<String, Option<Vec<u8>>>> {
    let data: BTreeMap<String, String> = match json {
        None => BTreeMap::new(),
        Some(json) => {
            let document: serde_json::Value =
                serde_json::from_str(json).context("parse Kubernetes Secret JSON")?;
            match document.get("data").cloned() {
                Some(data) => {
                    serde_json::from_value(data).context("parse Kubernetes Secret .data")?
                }
                None => BTreeMap::new(),
            }
        }
    };
    let mut values = BTreeMap::new();
    for key in keys {
        let decoded = data
            .get(key)
            .map(|encoded| STANDARD.decode(encoded))
            .transpose()
            .context("decode Kubernetes Secret value")?;
        values.insert(key.clone(), decoded);
    }
    Ok(values)
}

/// `navigator ops secrets apply --check` — decrypt in-process, the same
/// trust boundary [`apply`] uses, and report by name only whether each
/// projected object's SOPS value matches what is live in Secret Manager and
/// in the deployment's Kubernetes Secret.
///
/// Prints one line per object per comparison target — `match`, `differs`,
/// `missing in Secret Manager`, or `missing in K8s Secret` — and never a
/// value or a digest of one. Returns an error (which `main` turns into a
/// non-zero exit) when anything is not `match`. Changes nothing: Secret
/// Manager and the Kubernetes Secret are only ever read.
pub fn check_secrets(root: &Path, name: &str) -> Result<()> {
    let deployment = Deployment::load(root, name)?;

    let unsatisfied = unsatisfied_requirements(&deployment)?;
    if !unsatisfied.is_empty() {
        bail!(
            "{name} does not satisfy {} boot requirement(s) that apply to it: {}. Nothing was \
             checked.",
            unsatisfied.len(),
            unsatisfied
                .iter()
                .map(describe)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let (plan, _skipped) = plan(&deployment)?;
    let project_id = deployment.project_id().to_owned();
    let ship_config = ship::ShipConfig::from_deployment(&deployment)?;
    let decrypted = decrypt(&root.join(TREE).join(name).join(SECRETS_FILE))?;
    let payloads = resolve_payloads(&deployment, &plan, &decrypted)?;

    let k8s_json = fetch_k8s_secret_json(&ship_config)?;
    let k8s_values = parse_k8s_secret_values(k8s_json.as_deref(), payloads.keys())?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    let statuses = runtime.block_on(async {
        let token = gcp::auth::adc_token_provider().await?;
        let client = gcp::client::GcpClient::new(token);
        check_payloads(&client, &project_id, &payloads, &k8s_values).await
    })?;

    print!("{}", render_report(&statuses));

    let drifted: Vec<&str> = statuses
        .iter()
        .filter(|(_, (secret_manager, k8s))| {
            *secret_manager != CheckStatus::Match || *k8s != CheckStatus::Match
        })
        .map(|(object, _)| object.as_str())
        .collect();
    if !drifted.is_empty() {
        bail!(
            "{} object(s) drifted from {name}'s SOPS values: {}",
            drifted.len(),
            drifted.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_secret_payload_debug_names_keys_and_never_values() {
        let payload = KubernetesSecretPayload {
            name: "navigator-web-secrets",
            secret_type: "Opaque",
            data: BTreeMap::from([("TWILIO_AUTH_TOKEN".to_string(), "hunter2-value".to_string())]),
        };
        let shown = format!("{payload:?}");
        assert!(shown.contains("TWILIO_AUTH_TOKEN"), "{shown}");
        assert!(!shown.contains("hunter2-value"), "{shown}");
    }

    use std::path::PathBuf;

    use sha2::{Digest as _, Sha256};

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::gcp::client::{GcpClient, GcpService, StaticToken};
    use super::*;

    fn valid_unsatisfied_requirements(deployment: &Deployment) -> Vec<Requirement> {
        super::unsatisfied_requirements(deployment)
            .expect("synthetic deployment capability flags are valid")
    }

    fn gcp_client(server: &MockServer) -> GcpClient {
        GcpClient::new(std::sync::Arc::new(StaticToken("test-token".into())))
            .with_base_url(GcpService::SecretManager, server.uri())
    }

    #[test]
    fn classify_reports_match_differs_and_missing() {
        assert_eq!(
            classify(
                b"value",
                Some(b"value"),
                CheckStatus::MissingInSecretManager
            ),
            CheckStatus::Match
        );
        assert_eq!(
            classify(
                b"value",
                Some(b"other"),
                CheckStatus::MissingInSecretManager
            ),
            CheckStatus::Differs
        );
        assert_eq!(
            classify(b"value", None, CheckStatus::MissingInSecretManager),
            CheckStatus::MissingInSecretManager
        );
        assert_eq!(
            classify(b"value", None, CheckStatus::MissingInK8sSecret),
            CheckStatus::MissingInK8sSecret
        );
        // A length mismatch is `differs`, not a panic or an early-exit that
        // would skip comparing the rest.
        assert_eq!(
            classify(
                b"value",
                Some(b"a-longer-value"),
                CheckStatus::MissingInK8sSecret
            ),
            CheckStatus::Differs
        );
    }

    #[test]
    fn parse_k8s_secret_values_reports_present_and_missing_keys() {
        let json = serde_json::json!({
            "data": {
                "SESSION_SECRET": STANDARD.encode(b"live-value"),
            }
        })
        .to_string();
        let keys = ["SESSION_SECRET".to_string(), "OTHER_SECRET".to_string()];

        let values = parse_k8s_secret_values(Some(&json), keys.iter())
            .expect("valid Kubernetes Secret JSON parses");

        assert_eq!(
            values.get("SESSION_SECRET").unwrap().as_deref(),
            Some(b"live-value".as_slice())
        );
        assert_eq!(values.get("OTHER_SECRET").unwrap(), &None);
    }

    #[test]
    fn parse_k8s_secret_values_of_a_missing_secret_are_all_none() {
        let keys = ["SESSION_SECRET".to_string()];
        let values =
            parse_k8s_secret_values(None, keys.iter()).expect("a missing Secret is not an error");
        assert_eq!(values.get("SESSION_SECRET").unwrap(), &None);
    }

    #[tokio::test]
    async fn check_payloads_classifies_match_differs_and_both_kinds_of_missing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/v1/projects/example-project/secrets/MATCHES/versions/latest:access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "payload": { "data": STANDARD.encode(b"same-value") },
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/v1/projects/example-project/secrets/DRIFTED/versions/latest:access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "payload": { "data": STANDARD.encode(b"stale-value") },
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/v1/projects/example-project/secrets/ABSENT/versions/latest:access",
            ))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let mut payloads = BTreeMap::new();
        payloads.insert("MATCHES".to_string(), "same-value".to_string());
        payloads.insert("DRIFTED".to_string(), "fresh-value".to_string());
        payloads.insert("ABSENT".to_string(), "fresh-value".to_string());

        let mut k8s_values = BTreeMap::new();
        k8s_values.insert("MATCHES".to_string(), Some(b"same-value".to_vec()));
        k8s_values.insert("DRIFTED".to_string(), Some(b"fresh-value".to_vec()));
        // ABSENT deliberately has no entry: missing from the K8s Secret too.

        let statuses = check_payloads(
            &gcp_client(&server),
            "example-project",
            &payloads,
            &k8s_values,
        )
        .await
        .expect("every object resolves to a status");

        assert_eq!(
            statuses.get("MATCHES"),
            Some(&(CheckStatus::Match, CheckStatus::Match))
        );
        assert_eq!(
            statuses.get("DRIFTED"),
            Some(&(CheckStatus::Differs, CheckStatus::Match))
        );
        assert_eq!(
            statuses.get("ABSENT"),
            Some(&(
                CheckStatus::MissingInSecretManager,
                CheckStatus::MissingInK8sSecret
            ))
        );
    }

    #[test]
    fn render_report_never_carries_a_value_only_names_and_status_words() {
        let mut statuses = BTreeMap::new();
        statuses.insert(
            "SESSION_SECRET".to_string(),
            (CheckStatus::Differs, CheckStatus::MissingInK8sSecret),
        );

        let report = render_report(&statuses);

        assert!(report.contains("SESSION_SECRET (Secret Manager): differs"));
        assert!(report.contains("SESSION_SECRET (K8s Secret): missing in K8s Secret"));
        // The values a real caller would be comparing here — spelled out so
        // this test fails loudly if a future edit starts interpolating one.
        for leaked in ["do-not-leak-this-value", "stale-value", "fresh-value"] {
            assert!(!report.contains(leaked), "report leaked a value: {report}");
        }
    }

    /// Every deployment in the tree that declares itself provisioned.
    ///
    /// The secret-projection guards below exercise a row's `secrets.enc.yaml`,
    /// so they have nothing to check on a row whose cloud resources do not
    /// exist yet — it carries no encrypted file at all.
    fn provisioned_deployments(root: &Path) -> Vec<Deployment> {
        names(root)
            .expect("the deployments tree is readable")
            .into_iter()
            .map(|name| Deployment::load(root, &name).expect("the deployment loads"))
            .filter(|deployment| deployment.provisioned)
            .collect()
    }

    /// The workspace checkout, for tests that inspect repository files and
    /// resolve the public example tree.
    fn workspace() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the cli crate has a workspace parent")
            .to_path_buf()
    }

    /// The synthetic tree the unit gates read.
    ///
    /// Its rows exercise shapes the real rows do not hold — a declined
    /// integration, a scoped object — with synthetic values, so a test can
    /// assert on a value without touching key material. Operator deployment
    /// rows are supplied from an external root.
    fn fixture_tree() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("deployment-tree")
    }

    #[test]
    fn secrets_apply_writes_the_deployment_web_secret_without_exposing_values() {
        let deployment = Deployment::load(&fixture_tree(), "example-deployment")
            .expect("the synthetic deployment loads");
        let config = ship::ShipConfig::from_deployment(&deployment)
            .expect("the synthetic deployment resolves ship coordinates");
        let payloads: BTreeMap<String, String> = projected_objects()
            .into_iter()
            .map(|key| (key.clone(), format!("synthetic-{key}")))
            .collect();
        let expected_digests: BTreeMap<String, [u8; 32]> = payloads
            .iter()
            .map(|(key, value)| (key.clone(), Sha256::digest(value.as_bytes()).into()))
            .collect();
        let mut written = None;

        write_web_secret(&config, &payloads, |manifest| {
            written = Some(manifest.to_owned());
            Ok(())
        })
        .expect("the Kubernetes Secret is written");

        let written: serde_json::Value =
            serde_yaml::from_str(&written.expect("the secrets-apply writer receives a manifest"))
                .expect("the written Secret manifest parses");
        assert_eq!(
            written
                .pointer("/metadata/name")
                .and_then(serde_json::Value::as_str),
            Some("example-deployment-web-secrets")
        );
        assert_eq!(
            written
                .pointer("/metadata/namespace")
                .and_then(serde_json::Value::as_str),
            Some("example-deployment")
        );
        for (key, expected_digest) in expected_digests {
            let value = written
                .pointer(&format!("/stringData/{key}"))
                .and_then(serde_json::Value::as_str)
                .expect("every planned key reaches the Kubernetes Secret");
            assert!(
                Sha256::digest(value.as_bytes()).as_slice() == expected_digest,
                "the Kubernetes Secret value for {key} differs from the decrypted payload"
            );
        }
    }

    #[test]
    fn web_secret_apply_uses_server_side_apply_without_a_plaintext_last_applied_annotation() {
        assert!(WEB_SECRET_APPLY_ARGS.contains(&"--server-side"));
        assert!(!WEB_SECRET_APPLY_ARGS.contains(&"--save-config"));
    }

    /// Every literal `--deployment <name>` argument in `text`, as an operator
    /// would copy it. Placeholders (`<name>`, `$NAME`, `{name}`) and prose
    /// mentioning the flag are not instructions and are skipped: an argument
    /// counts only when it has the tree's `<word>-<word>` directory shape.
    fn instructed_deployments(text: &str) -> Vec<String> {
        let is_name = |token: &str| {
            token.contains('-')
                && !token.starts_with('-')
                && token
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        };
        let mut instructed = Vec::new();
        for (index, _) in text.match_indices("--deployment") {
            let tail = &text[index + "--deployment".len()..];
            // `--deployments-dir` starts with `--deployment` and is a different
            // flag taking a path. Without this the scan reads its tail as a
            // deployment called `s-dir`, which is a name no tree will ever hold
            // — a false positive that fails the build on a correct document.
            if !tail.starts_with(['=', ' ', '\t', '\n', '\r', '"', '\'']) && !tail.is_empty() {
                continue;
            }
            let tail = tail.strip_prefix('=').unwrap_or(tail);
            if tail.trim_start().starts_with(['<', '$', '{']) {
                continue;
            }
            let Some(token) = tail.split_whitespace().next() else {
                continue;
            };
            let token = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
            if is_name(token) {
                instructed.push(token.to_string());
            }
        }
        instructed
    }

    /// Every `--deployment <name>` a document or workflow instructs names a
    /// row that exists under `deployments/`.
    ///
    /// An operator copies the flag argument, so a renamed or invented row
    /// instructs a roll from a config that does not exist. Placeholders
    /// (`<row>`, `$NAME`) are skipped by `instructed_deployments`.
    #[test]
    fn every_instructed_deployment_names_a_real_row() {
        let root = workspace();
        let rows = names(&root).expect("the deployments tree is readable");
        let sources = [
            ".github/workflows/deploy.yml",
            ".github/workflows/release-deploy.yml",
            "docs/gitops.md",
            "docs/gke-prod.md",
            "docs/cronjobs.md",
            "docs/environments.md",
            "docs/deployment-secrets.md",
            "docs/manual-roll.md",
            "docs/release-automation.md",
            "docs/deploy/gke-ship-example.md",
            "server/content/workshops/navigator/DEPLOY.md",
        ];
        for source in sources {
            let text = fs::read_to_string(root.join(source))
                .unwrap_or_else(|error| panic!("read {source}: {error}"));
            for instructed in instructed_deployments(&text) {
                assert!(
                    rows.contains(&instructed),
                    "{source} instructs `--deployment {instructed}`, but deployments/ holds only \
                     {rows:?}"
                );
            }
        }
    }

    /// The public synthetic rows pass the same tree-level gate operators run:
    /// they load, have no plaintext beside them, their `.sops.yaml` rules name
    /// their projects' keys, and every boot requirement is satisfied.
    #[test]
    fn the_public_example_deployment_tree_passes_every_gate() {
        check(&fixture_tree())
            .expect("the synthetic deployment tree passes `navigator ops deployments`");
    }

    #[test]
    fn instructed_deployments_reads_literals_and_skips_placeholders() {
        let text = "run `navigator ops ship --deployment neon-law-test --tag 26.1.1`, then \
                    `navigator ops ship --deployment=acme-stg --dry-run`; the general \
                    form is `navigator ops ship --deployment <name>` or --deployment $NAME";
        assert_eq!(
            instructed_deployments(text),
            vec!["neon-law-test".to_string(), "acme-stg".to_string()]
        );
    }

    /// The CI parity gate. Every requirement that applies to a deployment must
    /// be satisfied by that deployment's own files.
    ///
    /// Deliberately not "every deployment carries every key": requirements
    /// carry an explicit capability scope, and the GitHub webhook pair is
    /// scoped to the marked automation home alone. Measuring every deployment against
    /// the full list would demand that singleton's receiver credentials
    /// everywhere, which is the opposite of what `store::deployment` says.
    ///
    /// Names only. This reads two files and never decrypts, so it needs no
    /// credential, no KMS permission, and no network.
    #[test]
    fn every_deployment_satisfies_the_requirements_that_apply_to_it() {
        let root = fixture_tree();
        let deployments = names(&root).expect("the deployments tree is readable");
        assert!(
            !deployments.is_empty(),
            "the tree must describe at least one deployment"
        );

        for name in deployments {
            let deployment = Deployment::load(&root, &name).expect("the deployment loads");
            // An unprovisioned row describes cloud resources nobody has created
            // yet, so its coordinates are deliberately blank. `ops ship` refuses
            // one outright; demanding completeness here would only force
            // placeholder values into the tree that read as real.
            if !deployment.provisioned {
                continue;
            }
            let missing = valid_unsatisfied_requirements(&deployment);
            assert!(
                missing.is_empty(),
                "{name} does not satisfy {}: {}. Add each key to \
                 deployments/{name}/{CONFIG_FILE} (a coordinate) or \
                 deployments/{name}/{SECRETS_FILE} (key material).",
                missing.len(),
                missing.iter().map(describe).collect::<Vec<_>>().join(", ")
            );
        }
    }

    /// The guard that a decrypted file can never be committed. `Deployment::load`
    /// refuses a plaintext value, so loading every deployment *is* the check —
    /// but a stray `secrets.yaml` beside the encrypted one would not be loaded
    /// at all, so the tree is walked for anything that looks like key material.
    #[test]
    fn no_plaintext_key_material_sits_in_the_tree() {
        let root = fixture_tree();
        for name in names(&root).expect("the deployments tree is readable") {
            let dir = root.join(TREE).join(&name);
            for entry in fs::read_dir(&dir).expect("a deployment directory is readable") {
                let path = entry.expect("a directory entry").path();
                let file_name = path
                    .file_name()
                    .expect("a file has a name")
                    .to_string_lossy()
                    .into_owned();
                if file_name == CONFIG_FILE {
                    continue;
                }
                assert_eq!(
                    file_name, SECRETS_FILE,
                    "deployments/{name}/{file_name} is neither the plaintext coordinates nor the \
                     encrypted key material. A decrypted file must never be written here — see \
                     .gitignore and docs/deployment-secrets.md."
                );
                encrypted_key_names(&fs::read_to_string(&path).expect("readable"))
                    .unwrap_or_else(|error| panic!("deployments/{name}/{file_name}: {error}"));
            }
        }
    }

    /// `sops` reads its key from `.sops.yaml`, and `config.toml` records the
    /// key the deployment is supposed to use. If they disagree, one
    /// deployment's key material is encrypted against another deployment's
    /// key — which is exactly the isolation this design turns on, failing
    /// silently, because both files still decrypt for whoever holds both keys.
    #[test]
    fn each_deployment_declares_the_kms_key_its_sops_rule_uses() {
        let root = fixture_tree();
        let rules = fs::read_to_string(root.join(".sops.yaml")).expect("read .sops.yaml");
        for name in names(&root).expect("the deployments tree is readable") {
            let deployment = Deployment::load(&root, &name).expect("the deployment loads");
            assert!(
                rules.contains(&deployment.kms_key),
                "no .sops.yaml creation rule names {name}'s key {}",
                deployment.kms_key
            );
            assert!(
                deployment.kms_key.contains(deployment.project_id()),
                "{name}'s key must live in its own project ({}), so staging's key is not \
                 decryptable by production's principals",
                deployment.project_id()
            );
        }
    }

    const ENCRYPTED: &str = r"
SESSION_SECRET: ENC[AES256_GCM,data:aaaa,iv:bbbb,tag:cccc,type:str]
NAVIGATOR_SURREAL_PASSWORD: ENC[AES256_GCM,data:dddd,iv:eeee,tag:ffff,type:str]
sops:
    kms: []
    gcp_kms:
        - resource_id: projects/example-project/locations/example-region/keyRings/navigator-secrets/cryptoKeys/deployment-config
    version: 3.13.3
";

    #[test]
    fn key_names_come_back_without_the_sops_metadata_block() {
        let names = encrypted_key_names(ENCRYPTED).expect("a sops document parses");
        assert_eq!(
            names,
            ["NAVIGATOR_SURREAL_PASSWORD", "SESSION_SECRET"]
                .map(String::from)
                .into()
        );
    }

    #[test]
    fn a_plaintext_value_beside_encrypted_ones_is_refused() {
        // The accident this guards: an operator adds a key by hand, forgets
        // `sops --encrypt --in-place`, and commits a live credential. It
        // cannot be un-committed afterwards — only rotated at the provider.
        let document = format!("{ENCRYPTED}\nSENDGRID_API_KEY: SG.a-real-looking-key\n");
        let error = encrypted_key_names(&document).expect_err("plaintext must be refused");
        let rendered = error.to_string();
        assert!(rendered.contains("SENDGRID_API_KEY"));
        assert!(rendered.contains("is not encrypted"));
        assert!(
            !rendered.contains("SG.a-real-looking-key"),
            "the guard must name the key, never echo the value it caught"
        );
    }

    #[test]
    fn an_unencrypted_document_is_refused_even_with_no_values_to_check() {
        let error = encrypted_key_names("SESSION_SECRET: ENC[AES256_GCM,data:a]\n")
            .expect_err("a document with no sops block was never encrypted");
        assert!(error.to_string().contains("never encrypted"));
    }

    #[test]
    fn a_whole_file_envelope_is_refused() {
        // `sops --encrypt` over a non-YAML input produces a single `data`
        // blob. It decrypts identically and destroys the reason for choosing
        // this design: a rotation would be one opaque line.
        let blob = r"
data: ENC[AES256_GCM,data:everything,iv:b,tag:c,type:str]
sops:
    version: 3.13.3
";
        let error = encrypted_key_names(blob).expect_err("a blob must be refused");
        assert!(error.to_string().contains("per value"));
    }

    fn deployment(entries: &[(&str, &str)], encrypted: &[&str]) -> Deployment {
        let mut coordinates = BTreeMap::from([
            (
                store::deployment::GITHUB_AUTOMATION_HOME_FLAG.to_owned(),
                "false".to_owned(),
            ),
            (
                store::deployment::SMS_TEST_RECIPIENTS_FLAG.to_owned(),
                "false".to_owned(),
            ),
        ]);
        for (key, value) in entries {
            coordinates.insert((*key).to_owned(), (*value).to_owned());
        }
        Deployment {
            name: "example".into(),
            kms_key: "projects/example/locations/example-region/keyRings/k/cryptoKeys/c".into(),
            provisioned: true,
            coordinates,
            encrypted_keys: encrypted.iter().map(|k| (*k).to_owned()).collect(),
            postgres_encrypted_keys: BTreeSet::new(),
            forgejo_encrypted_keys: BTreeSet::new(),
        }
    }

    fn service_deployment(service: ServiceSecrets) -> Deployment {
        let mut deployment = deployment(&[(PROJECT_ID, "example-project")], &complete_keys());
        let keys = service.keys().iter().map(|key| (*key).to_owned()).collect();
        match service {
            ServiceSecrets::Postgres => deployment.postgres_encrypted_keys = keys,
            ServiceSecrets::Forgejo => {
                deployment.forgejo_encrypted_keys = keys;
                deployment.coordinates.insert(
                    "NAVIGATOR_FORGEJO_ROOT_URL".into(),
                    "https://git.example.test/".into(),
                );
            }
        }
        deployment
    }

    fn synthetic_service_payloads(service: ServiceSecrets) -> BTreeMap<String, String> {
        service
            .keys()
            .iter()
            .map(|key| {
                let value = if *key == "NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET" {
                    "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY".to_owned()
                } else {
                    format!("synthetic-{key}")
                };
                ((*key).to_owned(), value)
            })
            .collect()
    }

    #[test]
    fn service_plan_requires_exactly_the_selected_services_encrypted_keys() {
        let mut postgres = service_deployment(ServiceSecrets::Postgres);
        assert_eq!(
            service_plan(&postgres, ServiceSecrets::Postgres)
                .unwrap()
                .len(),
            3
        );
        postgres
            .postgres_encrypted_keys
            .remove("NAVIGATOR_POSTGRES_TLS_PRIVATE_KEY");
        let error = service_plan(&postgres, ServiceSecrets::Postgres).unwrap_err();
        assert!(error
            .to_string()
            .contains("NAVIGATOR_POSTGRES_TLS_PRIVATE_KEY"));
        assert!(service_plan(&postgres, ServiceSecrets::Forgejo)
            .unwrap_err()
            .to_string()
            .contains("NAVIGATOR_FORGEJO_DATABASE_PASSWORD"));
    }

    #[test]
    fn postgres_service_manifest_contains_only_its_mounted_file_secrets() {
        let deployment = service_deployment(ServiceSecrets::Postgres);
        let payloads = synthetic_service_payloads(ServiceSecrets::Postgres);
        let keys = service_plan(&deployment, ServiceSecrets::Postgres).unwrap();
        let values = service_payloads(ServiceSecrets::Postgres, &keys, &payloads).unwrap();
        let secrets =
            build_service_secrets(ServiceSecrets::Postgres, &deployment, "navigator", &values)
                .unwrap();
        assert_eq!(
            secrets.iter().map(|secret| secret.name).collect::<Vec<_>>(),
            vec!["navigator-postgres-auth", "navigator-postgres-tls"]
        );
        let mut rendered = String::new();
        write_service_secrets("navigator", &secrets, |manifest| {
            rendered = manifest.to_owned();
            Ok(())
        })
        .unwrap();
        assert!(rendered.contains("name: navigator-postgres-auth"));
        assert!(rendered.contains("name: navigator-postgres-tls"));
        assert!(rendered.contains("tls.crt"));
        assert!(rendered.contains("tls.key"));
        assert!(!rendered.contains("web-secrets"));
        assert!(!rendered.contains("envFrom"));
    }

    fn postgres_with_client_logins() -> (Deployment, BTreeMap<String, String>) {
        let mut deployment = service_deployment(ServiceSecrets::Postgres);
        let mut payloads = synthetic_service_payloads(ServiceSecrets::Postgres);
        for key in POSTGRES_CLIENT_KEYS {
            deployment.postgres_encrypted_keys.insert((*key).to_owned());
            payloads.insert((*key).to_owned(), format!("synthetic-{key}"));
        }
        (deployment, payloads)
    }

    #[test]
    fn postgres_client_logins_are_a_complete_optional_set() {
        let (mut deployment, _) = postgres_with_client_logins();
        let keys = service_plan(&deployment, ServiceSecrets::Postgres).unwrap();
        assert_eq!(keys.len(), 7);
        assert_eq!(
            ServiceSecrets::Postgres.targets(&keys),
            vec![
                "navigator-postgres-auth",
                "navigator-postgres-tls",
                "navigator-postgres-app",
                "navigator-postgres-migrator",
                "navigator-postgres-backup",
            ]
        );
        deployment
            .postgres_encrypted_keys
            .remove("NAVIGATOR_POSTGRES_BACKUP_PASSWORD");
        let error = service_plan(&deployment, ServiceSecrets::Postgres).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("NAVIGATOR_POSTGRES_BACKUP_PASSWORD"),
            "a partial client set is refused by name: {error}"
        );
    }

    #[test]
    fn postgres_client_logins_project_one_secret_per_role_with_the_ca_bundle() {
        let (deployment, payloads) = postgres_with_client_logins();
        let keys = service_plan(&deployment, ServiceSecrets::Postgres).unwrap();
        let values = service_payloads(ServiceSecrets::Postgres, &keys, &payloads).unwrap();
        let secrets =
            build_service_secrets(ServiceSecrets::Postgres, &deployment, "navigator", &values)
                .unwrap();
        for (name, password) in POSTGRES_CLIENT_SECRETS {
            let secret = secrets
                .iter()
                .find(|secret| secret.name == *name)
                .unwrap_or_else(|| panic!("{name} must be projected"));
            assert_eq!(
                secret.data.keys().map(String::as_str).collect::<Vec<_>>(),
                vec!["ca.crt", "password"]
            );
            assert_eq!(secret.data["password"], payloads[*password]);
            assert_eq!(
                secret.data["ca.crt"],
                payloads["NAVIGATOR_POSTGRES_CA_CERT"]
            );
            assert!(
                !secret
                    .data
                    .values()
                    .any(|value| value == &payloads["NAVIGATOR_POSTGRES_PASSWORD"]),
                "{name} must never carry the bootstrap superuser password"
            );
        }
    }

    #[test]
    fn postgres_dbos_login_projects_its_own_secret_with_the_ca_bundle() {
        let (mut deployment, mut payloads) = postgres_with_client_logins();
        for key in POSTGRES_DBOS_KEYS {
            deployment.postgres_encrypted_keys.insert((*key).to_owned());
            payloads.insert((*key).to_owned(), format!("synthetic-{key}"));
        }
        let keys = service_plan(&deployment, ServiceSecrets::Postgres).unwrap();
        assert_eq!(
            ServiceSecrets::Postgres.targets(&keys).last(),
            Some(&POSTGRES_DBOS_SECRET)
        );
        let values = service_payloads(ServiceSecrets::Postgres, &keys, &payloads).unwrap();
        let secrets =
            build_service_secrets(ServiceSecrets::Postgres, &deployment, "navigator", &values)
                .unwrap();
        let secret = secrets
            .iter()
            .find(|secret| secret.name == POSTGRES_DBOS_SECRET)
            .expect("the DBOS login is projected");
        assert_eq!(
            secret.data["password"],
            payloads["NAVIGATOR_POSTGRES_DBOS_PASSWORD"]
        );
        assert_eq!(
            secret.data["ca.crt"],
            payloads["NAVIGATOR_POSTGRES_CA_CERT"]
        );

        let (deployment, _) = postgres_with_client_logins();
        let keys = service_plan(&deployment, ServiceSecrets::Postgres).unwrap();
        assert!(
            !ServiceSecrets::Postgres
                .targets(&keys)
                .contains(&POSTGRES_DBOS_SECRET),
            "a file without the DBOS login projects no DBOS Secret"
        );
    }

    #[test]
    fn forgejo_secret_manifest_renders_private_config_and_separate_database_ca() {
        let deployment = service_deployment(ServiceSecrets::Forgejo);
        let payloads = synthetic_service_payloads(ServiceSecrets::Forgejo);
        let keys = service_plan(&deployment, ServiceSecrets::Forgejo).unwrap();
        let values = service_payloads(ServiceSecrets::Forgejo, &keys, &payloads).unwrap();
        let secrets =
            build_service_secrets(ServiceSecrets::Forgejo, &deployment, "navigator", &values)
                .unwrap();
        assert_eq!(
            secrets.iter().map(|secret| secret.name).collect::<Vec<_>>(),
            vec![
                "navigator-forgejo-config",
                "navigator-forgejo-postgres-ca",
                "navigator-forgejo-database",
            ]
        );
        assert_eq!(
            ServiceSecrets::Forgejo.targets(&keys),
            secrets.iter().map(|secret| secret.name).collect::<Vec<_>>()
        );
        assert_eq!(
            secrets[2].data,
            BTreeMap::from([(
                "password".to_owned(),
                payloads["NAVIGATOR_FORGEJO_DATABASE_PASSWORD"].clone()
            )]),
            "the database Job's Secret carries only the forgejo login's password"
        );
        let app_ini = &secrets[0].data["app.ini"];
        assert!(app_ini.contains("ROOT_URL = https://git.example.test/"));
        assert!(app_ini.contains("HOST = navigator-postgres.navigator.svc.cluster.local:5432"));
        assert!(app_ini.contains(&payloads["NAVIGATOR_FORGEJO_DATABASE_PASSWORD"]));
        assert!(app_ini.contains("JWT_SECRET_URI = file:/etc/forgejo/oauth2_jwt_secret"));
        assert!(!app_ini.contains(&payloads["NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET"]));
        assert_eq!(
            secrets[0].data["oauth2_jwt_secret"],
            payloads["NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET"]
        );
        assert!(!app_ini.contains("__NAVIGATOR_"));
        assert_eq!(
            secrets[1].data["ca.crt"],
            payloads["NAVIGATOR_FORGEJO_POSTGRES_CA_CERT"]
        );
        assert!(!app_ini.contains("NAVIGATOR_POSTGRES_PASSWORD"));
        assert!(
            app_ini.contains("DISABLE_REGISTRATION = true")
                && app_ini.contains("ENABLE_AUTO_REGISTRATION = false")
                && app_ini.contains("EMAIL_DOMAIN_ALLOWLIST =\n"),
            "the deployment's base configuration keeps every registration path closed"
        );
    }

    #[test]
    fn forgejo_actions_requires_explicit_opt_in_and_instance_signing() {
        let mut deployment = service_deployment(ServiceSecrets::Forgejo);
        let payloads = synthetic_service_payloads(ServiceSecrets::Forgejo);
        let render = |deployment: &Deployment| {
            build_service_secrets(ServiceSecrets::Forgejo, deployment, "navigator", &payloads)
        };
        assert!(render(&deployment).unwrap()[0].data["app.ini"]
            .contains("[actions]\nENABLED = false\n"));
        deployment
            .coordinates
            .insert("NAVIGATOR_FORGEJO_ACTIONS_ENABLED".into(), "yes".into());
        assert!(
            render(&deployment).is_err(),
            "an invalid opt-in must fail closed"
        );
        deployment
            .coordinates
            .insert("NAVIGATOR_FORGEJO_ACTIONS_ENABLED".into(), "true".into());
        assert!(
            render(&deployment).is_err(),
            "Actions requires the signing prerequisites"
        );
    }

    #[test]
    fn forgejo_signing_projects_a_complete_key_pair_without_exposing_it_in_config() {
        let mut deployment = service_deployment(ServiceSecrets::Forgejo);
        let mut payloads = synthetic_service_payloads(ServiceSecrets::Forgejo);
        for (key, value) in [
            (
                "NAVIGATOR_FORGEJO_SIGNING_PRIVATE_KEY",
                "synthetic-private-signing-key",
            ),
            (
                "NAVIGATOR_FORGEJO_SIGNING_PUBLIC_KEY",
                "synthetic-public-signing-key",
            ),
        ] {
            deployment.forgejo_encrypted_keys.insert(key.into());
            payloads.insert(key.into(), value.into());
        }
        for (key, value) in [
            ("NAVIGATOR_FORGEJO_SIGNING_NAME", "Example Forge"),
            ("NAVIGATOR_FORGEJO_SIGNING_EMAIL", "forge@example.test"),
            ("NAVIGATOR_FORGEJO_ACTIONS_ENABLED", "true"),
        ] {
            deployment.coordinates.insert(key.into(), value.into());
        }
        let keys = service_plan(&deployment, ServiceSecrets::Forgejo).unwrap();
        let secrets =
            build_service_secrets(ServiceSecrets::Forgejo, &deployment, "navigator", &payloads)
                .unwrap();
        let signing = secrets
            .iter()
            .find(|secret| secret.name == "navigator-forgejo-signing")
            .expect("instance signing Secret");
        assert_eq!(signing.data["signing_key"], "synthetic-private-signing-key");
        assert_eq!(
            signing.data["signing_key.pub"],
            "synthetic-public-signing-key"
        );
        assert!(ServiceSecrets::Forgejo
            .targets(&keys)
            .contains(&"navigator-forgejo-signing"));
        let ini = &secrets[0].data["app.ini"];
        assert!(ini.contains("[actions]\nENABLED = true\n"));
        assert!(ini.contains("SIGNING_KEY = /etc/forgejo-signing/signing_key.pub"));
        assert!(ini.contains("SIGNING_NAME = Example Forge\nSIGNING_EMAIL = forge@example.test\n"));
        assert!(
            ini.contains("INITIAL_COMMIT = always\nCRUD_ACTIONS = always\nMERGES = always\n"),
            "server signing must not require a separate human key or second factor"
        );
        assert!(!ini.contains("synthetic-private-signing-key"));
        assert!(!ini.contains("synthetic-public-signing-key"));

        deployment.coordinates.insert(
            "NAVIGATOR_FORGEJO_SIGNING_NAME".into(),
            "Example\n[service]\nDISABLE_REGISTRATION=false".into(),
        );
        assert!(
            service_plan(&deployment, ServiceSecrets::Forgejo).is_err(),
            "signing identity must not inject configuration sections"
        );
        deployment.coordinates.insert(
            "NAVIGATOR_FORGEJO_SIGNING_NAME".into(),
            "Example Forge".into(),
        );

        deployment
            .forgejo_encrypted_keys
            .remove("NAVIGATOR_FORGEJO_SIGNING_PUBLIC_KEY");
        assert!(
            service_plan(&deployment, ServiceSecrets::Forgejo).is_err(),
            "partial signing pairs must be refused before reading credentials"
        );
    }

    fn forgejo_with_oidc() -> (Deployment, BTreeMap<String, String>) {
        let mut deployment = service_deployment(ServiceSecrets::Forgejo);
        let mut payloads = synthetic_service_payloads(ServiceSecrets::Forgejo);
        deployment
            .forgejo_encrypted_keys
            .insert(FORGEJO_OIDC_KEYS[0].to_owned());
        payloads.insert(
            FORGEJO_OIDC_KEYS[0].to_owned(),
            "synthetic-oidc-secret".to_owned(),
        );
        deployment.coordinates.insert(
            FORGEJO_OIDC_CLIENT_ID.to_owned(),
            "synthetic-forgejo-client".to_owned(),
        );
        (deployment, payloads)
    }

    #[test]
    fn forgejo_oidc_client_projects_its_own_secret() {
        let (deployment, payloads) = forgejo_with_oidc();
        let keys = service_plan(&deployment, ServiceSecrets::Forgejo).unwrap();
        let values = service_payloads(ServiceSecrets::Forgejo, &keys, &payloads).unwrap();
        let secrets =
            build_service_secrets(ServiceSecrets::Forgejo, &deployment, "navigator", &values)
                .unwrap();
        let oidc = secrets
            .iter()
            .find(|secret| secret.name == "navigator-forgejo-oidc")
            .expect("the OIDC client is projected");
        assert_eq!(
            oidc.data,
            BTreeMap::from([
                (
                    "client_id".to_owned(),
                    "synthetic-forgejo-client".to_owned()
                ),
                (
                    "client_secret".to_owned(),
                    "synthetic-oidc-secret".to_owned()
                ),
            ])
        );
        assert!(ServiceSecrets::Forgejo
            .targets(&keys)
            .contains(&"navigator-forgejo-oidc"));
        assert!(
            !secrets[0].data["app.ini"].contains("synthetic-oidc-secret"),
            "the client secret never enters app.ini"
        );
    }

    #[test]
    fn forgejo_oidc_secret_without_a_client_id_is_refused() {
        let (mut deployment, _) = forgejo_with_oidc();
        deployment.coordinates.remove(FORGEJO_OIDC_CLIENT_ID);
        let error = service_plan(&deployment, ServiceSecrets::Forgejo).unwrap_err();
        assert!(
            error.to_string().contains(FORGEJO_OIDC_CLIENT_ID),
            "{error}"
        );
    }

    #[test]
    fn forgejo_without_an_oidc_client_projects_no_oidc_secret() {
        let deployment = service_deployment(ServiceSecrets::Forgejo);
        let keys = service_plan(&deployment, ServiceSecrets::Forgejo).unwrap();
        assert!(!ServiceSecrets::Forgejo
            .targets(&keys)
            .contains(&"navigator-forgejo-oidc"));
    }

    #[test]
    fn forgejo_app_ini_requires_a_valid_32_byte_base64_jwt_secret() {
        let deployment = service_deployment(ServiceSecrets::Forgejo);
        let mut payloads = synthetic_service_payloads(ServiceSecrets::Forgejo);
        payloads.insert(
            "NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET".into(),
            "not-base64".into(),
        );
        let error =
            build_service_secrets(ServiceSecrets::Forgejo, &deployment, "navigator", &payloads)
                .unwrap_err();
        assert!(error.to_string().contains("must be base64-encoded"));
        assert!(!error.to_string().contains("not-base64"));
    }

    #[test]
    fn forgejo_app_ini_rejects_multiline_secret_injection() {
        let deployment = service_deployment(ServiceSecrets::Forgejo);
        let mut payloads = synthetic_service_payloads(ServiceSecrets::Forgejo);
        payloads.insert(
            "NAVIGATOR_FORGEJO_SECRET_KEY".into(),
            "line-one\nDISABLE_REGISTRATION = false".into(),
        );
        let error =
            build_service_secrets(ServiceSecrets::Forgejo, &deployment, "navigator", &payloads)
                .unwrap_err();
        assert!(error.to_string().contains("single line"));
        assert!(!error.to_string().contains("line-one"));
    }

    /// Every `WEB_REQUIREMENTS` alternative's first spelling, which is what a
    /// complete deployment carries.
    fn complete_keys() -> Vec<&'static str> {
        store::deployment::WEB_REQUIREMENTS
            .iter()
            .filter_map(|requirement| requirement.any_of.first())
            .flat_map(|alternative| alternative.iter().copied())
            .collect()
    }

    #[test]
    fn a_complete_deployment_has_no_unsatisfied_requirement() {
        let complete = complete_keys();
        let deployment = deployment(
            &[(store::deployment::GITHUB_AUTOMATION_HOME_FLAG, "true")],
            &complete,
        );
        let missing = valid_unsatisfied_requirements(&deployment);
        assert!(
            missing.is_empty(),
            "unsatisfied: {:?}",
            missing.iter().map(describe).collect::<Vec<_>>()
        );
    }

    /// A row that turns the summary lane on must supply every key the lane
    /// boots with; a row that leaves it off, or writes `"false"`, is asked for
    /// none. This is the gate 26.10.10-rc.1 needed: a key made mandatory in
    /// `EmailSummaryConfig` crash-looped staging because no row supplied it.
    #[test]
    fn an_enabled_summary_lane_demands_every_key_it_boots_with() {
        use workflows::email_summary_config::{
            CHANNEL_ENV, DEPLOYMENT_ENV, ENABLED_ENV, REQUIRED_KEYS,
        };

        let off = deployment(
            &[(PROJECT_ID, "neon-law-test"), (ENABLED_ENV, "false")],
            &[],
        );
        assert!(unsatisfied_summary_keys(&off).unwrap().is_empty());

        let supplied: Vec<&str> = REQUIRED_KEYS
            .iter()
            .copied()
            .filter(|key| *key != DEPLOYMENT_ENV && *key != CHANNEL_ENV)
            .collect();
        let without_channel = deployment(
            &[(PROJECT_ID, "neon-law-test"), (ENABLED_ENV, "true")],
            &supplied,
        );
        assert_eq!(
            unsatisfied_summary_keys(&without_channel).unwrap(),
            vec![CHANNEL_ENV],
            "the row's own name is rendered by `ops ship`; everything else must be supplied"
        );

        let typo = deployment(&[(PROJECT_ID, "neon-law-test"), (ENABLED_ENV, "yes")], &[]);
        assert!(unsatisfied_summary_keys(&typo).is_err());
    }

    /// The shape a signature-free row ships in: a deployment that executes no
    /// documents carries no `DOCUSIGN_*` at all and is still complete.
    ///
    /// This is the level `ops ship` measures at, so it is the level that has
    /// to agree with `portal`'s provider selection. Before `DocuSign` was
    /// trigger-gated, the only way past this preflight was a placeholder — and
    /// a placeholder selects the *real* provider, because
    /// `DocuSignSignatureProvider::from_env` accepts any non-empty value. The
    /// deployment would ship green and fail on its first signature request.
    #[test]
    fn a_deployment_that_signs_nothing_is_complete_without_docusign() {
        let without_docusign: Vec<&str> = complete_keys()
            .into_iter()
            .filter(|key| !key.starts_with("DOCUSIGN_"))
            .collect();
        let deployment = deployment(&[(PROJECT_ID, "neon-law-test")], &without_docusign);

        let missing = valid_unsatisfied_requirements(&deployment);
        assert!(
            missing.is_empty(),
            "a deployment declaring no DOCUSIGN_BASE_URL must be asked for no DocuSign key: {:?}",
            missing.iter().map(describe).collect::<Vec<_>>()
        );
    }

    /// The paired half at this level: declaring the integration and then
    /// half-supplying it is still a hard failure, so the gate cannot be
    /// escaped by naming the base URL alone.
    #[test]
    fn declaring_docusign_and_omitting_its_auth_is_still_unsatisfied() {
        // `DOCUSIGN_BASE_URL` is the declaration, not a requirement, so it is
        // absent from `complete_keys()` and has to be added deliberately —
        // which is exactly the operator action this test describes.
        let mut base_url_only: Vec<&str> = complete_keys()
            .into_iter()
            .filter(|key| !key.starts_with("DOCUSIGN_"))
            .collect();
        base_url_only.push("DOCUSIGN_BASE_URL");
        let deployment = deployment(&[(PROJECT_ID, "neon-law-test")], &base_url_only);

        let missing = valid_unsatisfied_requirements(&deployment);
        assert!(
            !missing.is_empty(),
            "DOCUSIGN_BASE_URL alone must not satisfy the DocuSign requirements"
        );
    }

    #[test]
    fn a_missing_required_key_is_reported() {
        // The drift this whole tree exists to make mechanical.
        let complete: Vec<&str> = complete_keys()
            .into_iter()
            .filter(|key| *key != "SENDGRID_API_KEY")
            .collect();
        let deployment = deployment(
            &[(store::deployment::GITHUB_AUTOMATION_HOME_FLAG, "true")],
            &complete,
        );
        let missing = valid_unsatisfied_requirements(&deployment);
        assert_eq!(
            missing.iter().map(describe).collect::<Vec<_>>(),
            vec!["SENDGRID_API_KEY"]
        );
    }

    #[test]
    fn the_restate_ingress_pair_is_required_of_the_automation_home_alone() {
        // The subtlety a "every deployment has every key" test gets wrong.
        // `store::deployment` scopes the trigger-submitted Restate ingress
        // pair to one project, so its absence is a gap there and correct
        // everywhere else.
        let without_ingress: Vec<&str> = complete_keys()
            .into_iter()
            .filter(|key| *key != "RESTATE_INGRESS_URL" && *key != "RESTATE_AUTH_TOKEN")
            .collect();

        let home = deployment(
            &[(store::deployment::GITHUB_AUTOMATION_HOME_FLAG, "true")],
            &without_ingress,
        );
        assert!(
            !valid_unsatisfied_requirements(&home).is_empty(),
            "the automation home must still need its Restate ingress credentials"
        );

        let elsewhere = deployment(&[], &without_ingress);
        assert!(
            valid_unsatisfied_requirements(&elsewhere).is_empty(),
            "no other deployment is preflighted for that singleton's ingress credentials"
        );
    }

    #[test]
    fn inline_env_keys_count_as_supplied_without_entering_the_encrypted_file() {
        // `NAVIGATOR_STORAGE_BACKEND` and friends are boot-required but ship
        // as inline Deployment env. Requiring them in the tree would push
        // four non-secrets into an encrypted file for no reason, so a
        // deployment that omits them entirely must still satisfy every
        // requirement.
        let without_inline: Vec<&str> = complete_keys()
            .into_iter()
            .filter(|key| !ship::INLINE_ENV_WEB_KEYS.contains(key))
            .collect();
        let deployment = deployment(&[(PROJECT_ID, "neon-law")], &without_inline);

        for key in ship::INLINE_ENV_WEB_KEYS {
            assert!(!deployment.encrypted_keys.contains(*key));
            assert!(deployment.supplied_keys().contains(*key));
        }
        assert!(valid_unsatisfied_requirements(&deployment).is_empty());
    }

    #[test]
    fn the_plan_names_every_projected_object_that_nothing_supplies() {
        let deployment = deployment(&[(PROJECT_ID, "neon-law")], &[]);
        let error = plan(&deployment).expect_err("an empty deployment supplies nothing");
        let rendered = error.to_string();
        assert!(rendered.contains("SESSION_SECRET"));
        assert!(rendered.contains("Secret Manager was not changed"));
    }

    #[test]
    fn a_production_row_is_not_asked_for_the_automation_home_s_ingress_credentials() {
        // The `SecretProviderClass` is one manifest rendered for every
        // deployment, so it references the trigger-submitted Restate ingress
        // pair everywhere. Demanding them would push a singleton's bearer
        // token into a row holding real client matters — the opposite of what
        // `store::deployment` scopes it to.
        let objects: Vec<String> = projected_objects().into_iter().collect();
        let home_only = ["RESTATE_INGRESS_URL", "RESTATE_AUTH_TOKEN"];
        let supplied: Vec<&str> = objects
            .iter()
            .map(String::as_str)
            .filter(|object| !home_only.contains(object))
            .collect();

        let (plan, skipped) = plan(&deployment(&[(PROJECT_ID, "neon-law")], &supplied))
            .expect("a production row without the receiver's credentials is complete");
        for key in home_only {
            assert!(!plan.contains_key(key));
            assert_eq!(
                skipped.get(key),
                Some(&Exemption::ScopedElsewhere),
                "{key} must be reported as skipped, never silently dropped"
            );
        }
    }

    /// The JWT triple every deployment signs into `DOCUSIGN_BASE_URL` with
    /// today, and which the `SecretProviderClass` therefore references.
    const DOCUSIGN_JWT_TRIPLE: [&str; 3] = [
        "DOCUSIGN_INTEGRATION_KEY",
        "DOCUSIGN_USER_ID",
        "DOCUSIGN_PRIVATE_KEY",
    ];

    #[test]
    fn an_alternative_the_deployment_satisfies_another_way_is_not_demanded() {
        // `WEB_REQUIREMENTS` models DocuSign auth as the JWT triple OR an
        // access token, and the manifest references only the triple. A
        // deployment that authenticates with a token instead must not be
        // asked for the three keys it correctly does not have.
        let objects: Vec<String> = projected_objects().into_iter().collect();
        let mut token_only: Vec<&str> = objects
            .iter()
            .map(String::as_str)
            .filter(|object| !DOCUSIGN_JWT_TRIPLE.contains(object))
            .collect();
        token_only.push("DOCUSIGN_ACCESS_TOKEN");

        let (plan, skipped) = plan(&deployment(&[(PROJECT_ID, "neon-law")], &token_only))
            .expect("an access token satisfies DocuSign auth");
        for key in DOCUSIGN_JWT_TRIPLE {
            assert!(!plan.contains_key(key));
            assert_eq!(
                skipped.get(key),
                Some(&Exemption::AlternativeSatisfied),
                "{key} must be reported as skipped, never silently dropped"
            );
        }
    }

    #[test]
    fn dropping_the_whole_docusign_alternative_set_is_still_missing() {
        // The other half: the exemption is "satisfied another way", not
        // "optional". Supply neither alternative and it is a real gap again.
        let objects: Vec<String> = projected_objects().into_iter().collect();
        let neither: Vec<&str> = objects
            .iter()
            .map(String::as_str)
            .filter(|object| !DOCUSIGN_JWT_TRIPLE.contains(object))
            .collect();

        let error = plan(&deployment(&[(PROJECT_ID, "neon-law")], &neither))
            .expect_err("no DocuSign auth at all is a gap");
        assert!(error.to_string().contains("DOCUSIGN_INTEGRATION_KEY"));
    }

    #[test]
    fn every_projected_object_is_written_by_some_deployment() {
        // The canary for a genuinely untrimmed manifest: an object no row
        // writes is a reference every rendered class drops, so it guards
        // nothing and only hides drift. `DOCUSIGN_ACCESS_TOKEN` sat in the
        // manifest exactly that way. No single row may be asked to write every
        // object: the automation home must not hold production-only social
        // app secrets, and a production row must not hold the automation
        // home's ingress credentials.
        let root = fixture_tree();
        let mut written = BTreeSet::new();
        for deployment in provisioned_deployments(&root) {
            let (plan, _) = plan(&deployment).expect("the tree supplies every projected object");
            written.extend(plan.into_keys());
        }
        let unwritten: Vec<String> = projected_objects()
            .into_iter()
            .filter(|object| !written.contains(object))
            .collect();
        assert!(
            unwritten.is_empty(),
            "the SecretProviderClass references object(s) no deployment writes: {unwritten:?}. \
             Trim each from examples/deploy/k8s/gke/secrets/secret-provider-class.yaml, or \
             supply a value in the row that carries it."
        );
    }

    #[test]
    fn a_deployment_that_declines_docusign_skips_every_docusign_object() {
        // The failure this whole seam exists for: a row that supplies no
        // `DOCUSIGN_BASE_URL`, so it declines the integration and runs
        // `StubSignatureProvider`. Every DocuSign object the shared manifest
        // references must read as a skip rather than a missing credential —
        // otherwise the only way to ship is a placeholder, which boots the real
        // provider instead of the stub.
        let root = fixture_tree();
        for deployment in provisioned_deployments(&root)
            .into_iter()
            .filter(|deployment| !deployment.coordinates.contains_key("DOCUSIGN_BASE_URL"))
        {
            let (plan, skipped) = plan(&deployment).expect("a declined integration is not a gap");
            for object in projected_objects()
                .into_iter()
                .filter(|object| object.starts_with("DOCUSIGN_"))
            {
                assert!(!plan.contains_key(&object), "{object} must not be written");
                assert_eq!(
                    skipped.get(&object),
                    Some(&Exemption::Untriggered),
                    "{object} belongs to an integration this deployment declares nothing of"
                );
            }
        }
    }

    const SOCIAL_OBJECTS: &[&str] = &[
        "NAVIGATOR_META_APP_ID",
        "NAVIGATOR_META_APP_SECRET",
        "NAVIGATOR_META_WEBHOOK_APP_SECRET",
        "NAVIGATOR_META_WEBHOOK_VERIFY_TOKEN",
        "NAVIGATOR_THREADS_APP_ID",
        "NAVIGATOR_THREADS_APP_SECRET",
        "NAVIGATOR_LINKEDIN_CLIENT_ID",
        "NAVIGATOR_LINKEDIN_CLIENT_SECRET",
        "NAVIGATOR_YOUTUBE_CLIENT_ID",
        "NAVIGATOR_YOUTUBE_CLIENT_SECRET",
        "NAVIGATOR_X_CLIENT_ID",
        "NAVIGATOR_X_CLIENT_SECRET",
        "NAVIGATOR_TIKTOK_BUSINESS_APP_ID",
        "NAVIGATOR_TIKTOK_BUSINESS_AUTHORIZATION_URL",
        "NAVIGATOR_TIKTOK_BUSINESS_APP_SECRET",
    ];

    #[test]
    fn a_deployment_that_declines_notion_skips_its_database_id() {
        let root = fixture_tree();
        for deployment in provisioned_deployments(&root)
            .into_iter()
            .filter(|deployment| {
                !deployment
                    .encrypted_keys
                    .contains("NAVIGATOR_NOTION_DATABASE_ID")
            })
        {
            let (plan, skipped) = plan(&deployment).expect("declining Notion is not a gap");
            assert!(!plan.contains_key("NAVIGATOR_NOTION_DATABASE_ID"));
            assert_eq!(
                skipped.get("NAVIGATOR_NOTION_DATABASE_ID"),
                Some(&Exemption::Untriggered),
                "the Notion database belongs to a lane this deployment declares nothing of"
            );
        }
    }

    #[test]
    fn runtime_kms_is_written_without_a_notion_database() {
        let root = fixture_tree();
        let deployment =
            Deployment::load(&root, "example-deployment").expect("the production row loads");
        assert!(!deployment
            .encrypted_keys
            .contains("NAVIGATOR_NOTION_DATABASE_ID"));
        let (plan, skipped) = plan(&deployment).expect("runtime KMS alone is not a gap");
        assert_eq!(
            plan.get("NAVIGATOR_RUNTIME_KMS_KEY"),
            Some(&Source::Encrypted)
        );
        assert!(skipped.contains_key("NAVIGATOR_NOTION_DATABASE_ID"));
    }

    #[test]
    fn a_deployment_that_declines_runtime_kms_skips_it() {
        let root = fixture_tree();
        let mut declined =
            Deployment::load(&root, "example-deployment").expect("the production row loads");
        for key in SOCIAL_OBJECTS {
            declined.coordinates.remove(*key);
        }
        for key in SOCIAL_OBJECTS.iter().chain(&["NAVIGATOR_RUNTIME_KMS_KEY"]) {
            declined.encrypted_keys.remove(*key);
        }
        let (plan, skipped) = plan(&declined).expect("declining runtime KMS is not a gap");
        assert!(!plan.contains_key("NAVIGATOR_RUNTIME_KMS_KEY"));
        assert_eq!(
            skipped.get("NAVIGATOR_RUNTIME_KMS_KEY"),
            Some(&Exemption::Untriggered)
        );
    }

    #[test]
    fn a_notion_database_without_runtime_kms_is_a_gap() {
        let root = fixture_tree();
        let mut notion_only =
            Deployment::load(&root, "example-deployment").expect("the production row loads");
        notion_only
            .encrypted_keys
            .insert("NAVIGATOR_NOTION_DATABASE_ID".to_string());
        for key in SOCIAL_OBJECTS {
            notion_only.coordinates.remove(*key);
        }
        for key in SOCIAL_OBJECTS.iter().chain(&["NAVIGATOR_RUNTIME_KMS_KEY"]) {
            notion_only.encrypted_keys.remove(*key);
        }
        let error =
            plan(&notion_only).expect_err("a Notion database without a runtime KMS key is a gap");
        assert!(error.to_string().contains("NAVIGATOR_RUNTIME_KMS_KEY"));
    }

    #[test]
    fn the_production_row_writes_every_enabled_social_object() {
        let root = fixture_tree();
        let deployment =
            Deployment::load(&root, "example-deployment").expect("the production row loads");
        assert!(valid_unsatisfied_requirements(&deployment).is_empty());
        let (plan, skipped) = plan(&deployment).expect("a complete social lane is not a gap");
        for object in SOCIAL_OBJECTS {
            let expected = if object.ends_with("_APP_ID")
                || object.ends_with("_CLIENT_ID")
                || object.ends_with("_AUTHORIZATION_URL")
            {
                Source::Coordinate
            } else {
                Source::Encrypted
            };
            assert_eq!(plan.get(*object), Some(&expected), "{object}");
            assert!(!skipped.contains_key(*object), "{object}");
        }
    }

    #[test]
    fn a_deployment_that_declines_social_apps_skips_every_social_object() {
        let root = fixture_tree();
        let deployment = Deployment::load(&root, "example-automation-home")
            .expect("the automation-home row loads");
        let (plan, skipped) = plan(&deployment).expect("declining social apps is not a gap");
        for object in SOCIAL_OBJECTS {
            assert!(!plan.contains_key(*object), "{object} must not be written");
            assert_eq!(
                skipped.get(*object),
                Some(&Exemption::Untriggered),
                "{object} belongs to an integration this deployment declares nothing of"
            );
        }
    }

    #[test]
    fn a_declared_social_app_missing_any_secret_is_a_gap() {
        let root = fixture_tree();
        for key in [
            "NAVIGATOR_META_APP_SECRET",
            "NAVIGATOR_META_WEBHOOK_VERIFY_TOKEN",
            "NAVIGATOR_META_WEBHOOK_APP_SECRET",
            "NAVIGATOR_THREADS_APP_SECRET",
            "NAVIGATOR_LINKEDIN_CLIENT_SECRET",
            "NAVIGATOR_YOUTUBE_CLIENT_SECRET",
            "NAVIGATOR_X_CLIENT_SECRET",
            "NAVIGATOR_TIKTOK_BUSINESS_APP_SECRET",
            "NAVIGATOR_RUNTIME_KMS_KEY",
        ] {
            let mut partial =
                Deployment::load(&root, "example-deployment").expect("the production row loads");
            partial.encrypted_keys.remove(key);
            let unsatisfied = valid_unsatisfied_requirements(&partial)
                .iter()
                .map(describe)
                .collect::<Vec<_>>();
            assert!(
                unsatisfied.contains(&key.to_owned()),
                "{key} must be demanded of a row that declares its social app: {unsatisfied:?}"
            );
            let error = plan(&partial).expect_err("a half-configured social lane is a gap");
            assert!(error.to_string().contains(key), "{error}");
        }
    }

    #[test]
    fn a_social_app_secret_without_its_app_id_is_a_gap() {
        let root = fixture_tree();
        let mut orphan =
            Deployment::load(&root, "example-deployment").expect("the production row loads");
        orphan.coordinates.remove("NAVIGATOR_THREADS_APP_ID");
        let unsatisfied = valid_unsatisfied_requirements(&orphan)
            .iter()
            .map(describe)
            .collect::<Vec<_>>();
        assert_eq!(unsatisfied, vec!["NAVIGATOR_THREADS_APP_ID".to_owned()]);
    }

    #[test]
    fn a_deployment_that_declines_dash0_skips_its_token() {
        let root = fixture_tree();
        for deployment in provisioned_deployments(&root)
            .into_iter()
            .filter(|deployment| !deployment.coordinates.contains_key("DASH0_ENDPOINT"))
        {
            let (plan, skipped) = plan(&deployment).expect("a declined integration is not a gap");
            assert!(!plan.contains_key("DASH0_TOKEN"));
            assert_eq!(
                skipped.get("DASH0_TOKEN"),
                Some(&Exemption::Untriggered),
                "DASH0_TOKEN belongs to an integration this deployment declares nothing of"
            );
        }
    }

    #[test]
    fn declaring_dash0_without_its_token_is_still_missing_the_token() {
        let root = fixture_tree();
        let mut deployment = Deployment::load(&root, "example-automation-home")
            .expect("the synthetic Dash0 deployment loads");
        deployment.encrypted_keys.remove("DASH0_TOKEN");

        let error =
            plan(&deployment).expect_err("a declared integration without its token is a gap");
        assert!(error.to_string().contains("DASH0_TOKEN"));
    }

    #[test]
    fn declaring_docusign_still_demands_every_key_the_provider_reads() {
        // The other half: `Untriggered` is "declined", never "optional". A
        // deployment that sets the trigger and stops there is half-configured,
        // which is the case the trigger was introduced to keep failing.
        let objects: Vec<String> = projected_objects().into_iter().collect();
        let base_url_only: Vec<&str> = objects
            .iter()
            .map(String::as_str)
            .filter(|object| !object.starts_with("DOCUSIGN_") || *object == "DOCUSIGN_BASE_URL")
            .collect();

        let error = plan(&deployment(&[(PROJECT_ID, "neon-law")], &base_url_only))
            .expect_err("declaring DocuSign and supplying nothing else is a gap");
        for key in [
            "DOCUSIGN_ACCOUNT_ID",
            "DOCUSIGN_HMAC_KEY",
            "DOCUSIGN_OAUTH_BASE",
            "DOCUSIGN_SIGNER_EMAIL",
        ] {
            assert!(error.to_string().contains(key), "{key} must still be named");
        }
    }

    #[test]
    fn the_automation_home_is_still_asked_for_its_own_ingress_credentials() {
        // The other half: the exemption is scoped to the project that does not
        // own the Restate ingress pair, never a blanket allowance.
        let objects: Vec<String> = projected_objects().into_iter().collect();
        let supplied: Vec<&str> = objects
            .iter()
            .map(String::as_str)
            .filter(|object| *object != "RESTATE_INGRESS_URL")
            .collect();

        let error = plan(&deployment(
            &[(store::deployment::GITHUB_AUTOMATION_HOME_FLAG, "true")],
            &supplied,
        ))
        .expect_err("the automation home owns the ingress pair");
        assert!(error.to_string().contains("RESTATE_INGRESS_URL"));
    }

    #[test]
    fn an_unscoped_object_is_never_exempt() {
        let nothing = BTreeSet::new();
        // `SESSION_SECRET` is required of every deployment, so no project may
        // skip it however the rendering manifest is shaped.
        assert_eq!(
            exemption(
                "SESSION_SECRET",
                store::deployment::DeploymentCapabilities::default(),
                &nothing
            ),
            None
        );
        // …and neither is an object no requirement mentions at all: a Drive
        // folder id is projected everywhere on purpose, so its absence is a
        // real gap rather than a scoping decision.
        assert_eq!(
            exemption(
                "NAVIGATOR_DRIVE_NEON_LAW_PROJECTS_DRIVE_ID",
                store::deployment::DeploymentCapabilities::default(),
                &nothing
            ),
            None
        );
    }

    #[test]
    fn a_coordinate_supplies_a_projected_object_the_encrypted_file_does_not() {
        // Not everything the SecretProviderClass projects is a secret. The
        // Drive folder ids and `NAVIGATOR_FORGE_BACKEND` are coordinates that
        // ride the Secret rail because the pod reads one Secret, not two
        // sources — so the plan must accept them from `config.toml`.
        let objects: Vec<String> = projected_objects().into_iter().collect();
        let encrypted: Vec<&str> = objects
            .iter()
            .filter(|object| *object != "NAVIGATOR_FORGE_BACKEND")
            .map(String::as_str)
            .collect();
        let deployment = deployment(
            &[
                (PROJECT_ID, "neon-law"),
                ("NAVIGATOR_FORGE_BACKEND", "github"),
            ],
            &encrypted,
        );

        let (plan, skipped) = plan(&deployment).expect("a coordinate satisfies the object");
        assert_eq!(plan["NAVIGATOR_FORGE_BACKEND"], Source::Coordinate);
        assert_eq!(plan["SESSION_SECRET"], Source::Encrypted);
        assert!(skipped.is_empty());
    }

    // ---------- locating the tree ----------

    /// A checkout that carries the tree and nothing else: no `Cargo.toml`, no
    /// `k8s/`, so `orchestrate::workspace_root` cannot find it.
    fn deploy_only_checkout() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("create a temp checkout");
        fs::create_dir_all(dir.path().join(TREE).join("neon-law-test")).expect("create the tree");
        dir
    }

    #[test]
    fn an_explicit_directory_locates_a_tree_the_workspace_walk_cannot_find() {
        // The blocker the flag exists for. The tree does not have to live
        // beside the source that reads it, and the private deploy checkout it
        // will live in has neither of the two markers the walk looks for.
        let checkout = deploy_only_checkout();
        assert_eq!(
            root(Some(checkout.path())).expect("the explicit directory holds the tree"),
            checkout.path()
        );
        assert_eq!(
            names(checkout.path()).expect("the tree is readable"),
            vec!["neon-law-test".to_owned()]
        );
    }

    #[test]
    fn being_handed_the_tree_itself_says_so_and_names_the_fix() {
        // The reading the flag's name invites. Left alone it produces a
        // `no deployment <name> in deployments/` error describing the wrong
        // problem, and the fix — pass the parent — is not derivable from it.
        let checkout = deploy_only_checkout();
        let error = root(Some(&checkout.path().join(TREE)))
            .expect_err("the tree itself is not the directory containing it");
        let message = error.to_string();
        assert!(
            message.contains("the directory CONTAINING the tree"),
            "the error must say what it wanted: {message}"
        );
        assert!(
            message.contains(&checkout.path().display().to_string()),
            "the error must name the directory to pass instead: {message}"
        );
    }

    #[test]
    fn a_directory_with_no_tree_in_it_is_refused_at_the_flag() {
        let empty = tempfile::tempdir().expect("create a temp directory");
        let error = root(Some(empty.path())).expect_err("there is no tree here");
        assert!(
            error.to_string().contains("no `deployments/` directory"),
            "{error}"
        );
    }

    #[test]
    fn the_workspace_resolves_its_own_tree() {
        if env::var_os(ROOT_ENV).is_some() {
            eprintln!("{ROOT_ENV} is set; the workspace fallback is not what resolves here");
            return;
        }
        assert_eq!(
            root(None).expect("the workspace holds deployments/"),
            workspace(),
            "with no flag and no {ROOT_ENV}, the tree is this checkout's own deployments/"
        );
    }

    /// The fixture is reached the same way an operator reaches the real tree:
    /// through `root`, with an explicit directory. Nothing about the resolver
    /// is special-cased for it.
    #[test]
    fn the_fixture_tree_resolves_through_the_ordinary_flag() {
        let root = root(Some(&fixture_tree())).expect("the fixture holds a tree");
        assert_eq!(
            names(&root).expect("the tree is readable"),
            vec![
                "example-automation-home".to_owned(),
                "example-deployment".to_owned()
            ]
        );
    }
}

#[cfg(test)]
mod record_apply_tests {
    use base64::engine::general_purpose::STANDARD;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::gcp::client::{GcpClient, GcpService, StaticToken};
    use super::*;

    fn row() -> Deployment {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deployment-tree");
        Deployment::load(&root, "example-deployment").unwrap()
    }

    fn client(server: &MockServer) -> GcpClient {
        GcpClient::new(std::sync::Arc::new(StaticToken("synthetic".into())))
            .with_base_url(GcpService::SecretManager, server.uri())
    }

    #[tokio::test]
    async fn record_payloads_read_secrets_and_write_only_coordinates() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/v1/projects/example-project/secrets/TEST_TOKEN/versions/latest:access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "payload": {"data": STANDARD.encode("synthetic-value")}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(2)
            .mount(&server)
            .await;
        let plan = BTreeMap::from([
            ("TEST_TOKEN".to_owned(), Source::Encrypted),
            ("NAVIGATOR_PUBLIC_HOST".to_owned(), Source::Coordinate),
        ]);
        let payloads = record_payloads(&client(&server), &row(), &plan)
            .await
            .unwrap();
        assert_eq!(payloads["TEST_TOKEN"], "synthetic-value");
        assert_eq!(
            payloads["NAVIGATOR_PUBLIC_HOST"],
            "navigator.example.invalid"
        );
        let requests = server.received_requests().await.unwrap();
        assert!(requests
            .iter()
            .filter(|request| request.method.as_str() == "POST")
            .all(|request| request.url.as_str().contains("NAVIGATOR_PUBLIC_HOST")));
    }

    #[tokio::test]
    async fn a_secret_missing_from_secret_manager_names_the_command_that_sets_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let plan = BTreeMap::from([("TEST_TOKEN".to_owned(), Source::Encrypted)]);
        let error = record_payloads(&client(&server), &row(), &plan)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("TEST_TOKEN"), "{error}");
        assert!(error.contains("ops secrets set"), "{error}");
    }

    #[test]
    fn record_apply_dry_run_reads_nothing() {
        apply_from_record(&row(), None, true).unwrap();
    }
}

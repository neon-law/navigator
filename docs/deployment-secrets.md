# Deployment configuration and secrets — an external `deployments/` tree

A row's published record is its only source; see [the published record](#the-published-record). The private tree
described first below is how a row reaches the record once, and how its SOPS values reach Secret Manager until they are
set there directly.

Each operator supplies deployment coordinates and encrypted key material from an approved private configuration root.
Coordinates are plaintext; key material is encrypted per value against that deployment's own Google Cloud KMS key. A
command decrypts and writes Google Secret Manager and the Kubernetes Secret the pod reads.

```text
deployments/ (SOPS) --> ops secrets apply --> Secret Manager
                                   \--> <name>-web-secrets --> pod envFrom
```

## Where the tree lives

The tree is a directory named `deployments/` beneath an operator-managed root outside this source checkout. Set
`NAVIGATOR_DEPLOYMENTS_DIR` to that root, or pass it with `--deployments-dir`; commands never infer operational rows
from the repository. The repository contains only synthetic examples in
`cli/tests/fixtures/deployment-tree/deployments/`. Configure `.sops.yaml` and the key-name inventory alongside the
external tree, and keep the denylist supplied through `NAVIGATOR_DEPLOYMENT_COORDINATES_DENYLIST` outside Git. A roll
uses the released `navigator` binary at the exact tag being shipped, because the GKE manifests are compiled into it; see
[`manual-roll.md`](manual-roll.md).

CI runs source-tree checks against synthetic rows only. Those checks validate parser and gate behavior; they do not
prove that an operator's external rows are valid, that a KMS key is available, or that a deployment is ready. Re-run
`navigator ops deployments --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR"` and the SOPS inventory checks against the
approved external root before publication and after any row change. Keep output limited to aggregate results.

When migrating an existing row, explicitly preserve whether it owns the GitHub automation home and/or SMS test recipient
requirements by setting the corresponding capability flags in that external row. The flags replace identity-by-project
inference; they are not inferred automatically. Do not roll the row until its operator has confirmed the intended
requirement profile and the external-root preflight passes. Synthetic CI cannot prove this migration.

The application never reads this tree and never reads GitHub. It reads the projected Kubernetes Secret. This page is the
operator boundary; the deployment map itself is [`environments.md`](environments.md).

## The published record

The tree is a one-time import source. A row's source of truth is its published record,
`navigator-deployment-config-<row>` in the row's own Secret Manager: coordinates and secret key names, never a value.
`ops ship --project-id` reads it, and so do `ops secrets apply`, `ops observability`, `ops logs`, and `ops
brand-readiness` when given `--project-id`. [`manual-roll.md`](manual-roll.md#the-published-record-is-the-row) lists
what the record derives and the `ops deployments init`, `set`, `unset`, and `import` verbs that write it.

```text
ops deployments init|set|unset|import --> navigator-deployment-config-<row>   (coordinates + key names)
ops secrets set|unset                 --> Secret Manager <KEY>                (values) + the record's key names
ops secrets apply --project-id        --> <name>-web-secrets / service Secrets --> pod
```

Once a row is in the record, supply a value straight to Secret Manager. The value is read from stdin, never argv, and is
never printed; one trailing newline is stripped unless `--raw` is passed. The key name is added to the record's web
inventory, or the PostgreSQL or Forgejo inventory with `--service`:

```bash
printf '%s' "$VALUE" | navigator ops secrets set <row> --project-id <project> HCAPTCHA_SECRET
navigator ops secrets set <row> --project-id <project> NAVIGATOR_POSTGRES_PASSWORD --service postgres < file
navigator ops secrets unset <row> --project-id <project> HCAPTCHA_SECRET
```

`unset` removes the key name from the record. Secret Manager keeps the stored versions, and nothing projects them once
the name is gone; destroy a version in Secret Manager only after the provider credential is revoked.

Project the Kubernetes Secrets from the record and Secret Manager:

```bash
navigator ops secrets apply --deployment <row> --project-id <project> --dry-run
navigator ops secrets apply --deployment <row> --project-id <project>
navigator ops secrets apply --deployment <row> --project-id <project> --service postgres
```

The web apply writes each coordinate-sourced object to Secret Manager, reads every secret object's `versions/latest`,
and applies `<name>-web-secrets`; a key with no value fails naming `ops secrets set`. The service apply reads only that
service's keys. `--check` compares SOPS with Secret Manager and so works only with `--deployments-dir`.

## Rotation revokes nothing unless you rotate at the provider

Read this before the layout, because it is the one thing the design cannot enforce for you.

Re-encrypting a file rotates the data key. **It revokes nothing.** Every prior ciphertext stays readable to anyone
holding repository history and the KMS key, because the key decrypts history and not just `HEAD`. A rotation is
therefore two steps in this order:

1. **Rotate the value at the provider** — issue a new SendGrid API key, a new DocuSign key pair, a new database
   password. This is the step that actually revokes the old credential.
2. **Re-encrypt the new value here**, and ship it.

Doing only step 2 leaves the old credential live and readable. Doing only step 1 leaves the deployment broken. There is
no third option in which the file edit alone is sufficient.

Operationally, one rotation is one deployment at a time, never two in one sitting:

1. Rotate the value at the provider.
2. Re-encrypt it here: the secure stdin command below.
3. Apply the updated encrypted value with `navigator ops secrets apply`; it writes a new version.
4. `navigator ops ship --deployment <name> --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR" --restart-only` restarts pods:
   nothing re-reads the Secret until they are recreated.
5. Verify that deployment's `/app/readyz`, `/version`, and the affected provider flow.

Never copy a value between deployments. Shared vendors still receive separate per-deployment credentials, and every
runtime project uses a deployment-specific Secret.

The corollary is to keep as little long-lived material in the tree as possible. The GitHub-to-GCP path uses Workload
Identity Federation and holds no stored credential; every value that can be a short-lived federated token instead of a
stored secret should be.

## Layout

```text
deployments/<name>/config.toml        coordinates, plaintext, reviewable
deployments/<name>/secrets.enc.yaml   key material, SOPS, per-deployment KMS key
deployments/<name>/postgres-secrets.enc.yaml  PostgreSQL service-only key material
deployments/<name>/forgejo-secrets.enc.yaml   Forgejo service-only key material
.sops.yaml                            which KMS key encrypts which deployment
```

One directory is one deployment. `config.toml` also records the `kms_key` that deployment's material is encrypted
against; `.sops.yaml` carries the matching creation rule, and a test fails the build if the two disagree.

Two properties are load-bearing rather than incidental:

- **Per-value encryption, never a whole-file blob.** `encrypted_regex` in `.sops.yaml` leaves the key names in plaintext
  and encrypts only the values. That is what makes a rotation a one-line diff naming the variable that changed, and what
  lets the parity gate read names in CI with no credential and no decrypt. An envelope over the whole file decrypts
  identically and destroys both.
- **One KMS key per deployment, in that deployment's own project.** No deployment's key is decryptable by another
  deployment's principals. The IAM binding lives on the key itself, inside the project. Never grant it at the
  organization level: organization-inherited bindings are lost when a project moves between organizations, which would
  silently change who can read the archive.

## What is a coordinate and what is key material

If publishing the value in a pull request would be a disclosure, it is key material. Otherwise it is a coordinate.

Project IDs, cluster and namespace names, bucket names, hostnames, the Workspace login domain, Drive folder and shared
Drive IDs, the GitHub App ID and bot login, and the SurrealDB namespace and database are coordinates. Database URLs,
session and webhook signing secrets, OAuth client secrets, API keys, and private keys are key material.

Four boot-required keys are in neither file. `NAVIGATOR_CLAMD_ADDR`, `NAVIGATOR_STORAGE_BACKEND`,
`NAVIGATOR_EMAIL_BACKEND`, and `GOOGLE_OAUTH_CLIENT_IDS` ship as inline Deployment env — `ship::INLINE_ENV_WEB_KEYS`
records that, and the parity gate reads the same list, so the two cannot drift.

Not everything on the Secret rail is boot-required, and the ones that are not are the ones worth checking by hand. A key
absent from `WEB_REQUIREMENTS` fails no gate, so if the pod reads it with a default, the loss is silent rather than
loud. `DOCUSIGN_SIGNER_EMAIL` is the worked example: unset, `portal::retainer_walk` falls back to `support@<deployment>`
and envelopes keep going out addressed to the firm's shared mailbox rather than the deployment's signer. It is projected
for that reason, not because any invariant demands it.

## Provision a deployment's key

Once per deployment, before its `secrets.enc.yaml` exists, supply the operator's explicit IAM member to setup:

```bash
navigator ops gcp setup --project-id <project> --public-base-url https://www.example.test \
    --region <region> --kms-operator user:operator@example.com
```

Setup creates the deployment's `navigator-secrets/deployment-config` key and grants that member
`roles/cloudkms.cryptoKeyEncrypterDecrypter` on the key alone. SOPS needs both permissions to wrap and unwrap its data
key. The grant preserves other bindings and policy conditions. An existing unconditional grant needs no policy write.
`--kms-operator` accepts an explicit `user:`, `serviceAccount:`, or `group:` email member. It has no environment
fallback and never infers the signed-in identity. Omit it to leave operator IAM unchanged. Public, domain-wide, and
blank principals fail before setup makes any cloud request. `--dry-run` previews the same key-scoped policy change
without authentication or network traffic.

Grant no runtime service account on this key. `ops secrets apply` performs the decrypt and writes the Kubernetes Secret;
nothing in a cluster decrypts a repository file.

Prove the isolation rather than assuming it, by enumerating every path that grants access to a `CryptoKey` — the key
itself, the keyring above it, the project, and the organization. An attempted decrypt under an impersonated service
account is the tempting test and the wrong one: unless the operator holds `roles/iam.serviceAccountTokenCreator` on that
account, it fails at `iam.serviceAccounts.getAccessToken` before it ever reaches the KMS permission, so the
`PERMISSION_DENIED` says nothing about the key. Enumeration also covers principals nobody thought to test.

```bash
: "${PROJECT_ID:?Set this from the selected external deployment config}"
: "${GCP_LOCATION:?Set this from the selected external deployment config}"
: "${ORGANIZATION_ID:?Set this from the approved operator configuration}"

gcloud kms keys get-iam-policy deployment-config \
    --project "$PROJECT_ID" --location "$GCP_LOCATION" --keyring navigator-secrets

gcloud kms keyrings get-iam-policy navigator-secrets \
    --project "$PROJECT_ID" --location "$GCP_LOCATION"

gcloud projects get-iam-policy "$PROJECT_ID" \
    --flatten="bindings[].members" --filter="bindings.role:cloudkms" \
    --format="table(bindings.role, bindings.members)"

# Substitute the organization from `gcloud projects get-ancestors "$PROJECT_ID"`,
# and check any folder between the two if one exists.
gcloud organizations get-iam-policy "$ORGANIZATION_ID" \
    --flatten="bindings[].members" --filter="bindings.role:cloudkms" \
    --format="table(bindings.role, bindings.members)"
```

Passing means the key policy names the operator and nothing else, and the other three return no `cloudkms` binding at
all. A runtime service account appearing at any level means that deployment's identity can read the other's archive, so
the bindings need correcting before any value is encrypted. An organization-level result is doubly wrong: it grants
access and it disappears on a project move.

Keyrings and keys cannot be deleted, only disabled, so the location and names above are effectively permanent.

## Author or edit key material

`sops` opens a decrypted temporary file in `$EDITOR` and re-encrypts on save. An editor may leave swap, backup, or
recovery files; use the stdin method for production social secrets:

```bash
sops deployments/<deployment>/secrets.enc.yaml
```

For a value that must not pass through an editor buffer — a private key PEM, a database URL — set it from a pipe:

```bash
openssl rand -hex 32 | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read().strip()))' | \
    sops set --value-stdin deployments/<name>/secrets.enc.yaml '["SESSION_SECRET"]'
```

Committing a decrypted file is the one unrecoverable mistake here, so two guards sit in front of it. `.gitignore`
refuses the filenames a decrypted working copy usually takes, and `navigator ops deployments` fails if any value in the
tree is not `ENC[…]` — including a file that was never encrypted at all. Neither guard can help after a push: at that
point the value is disclosed and the only remedy is rotating it at the provider.

## Apply secrets

Dry run first. It reads names only — it does not shell out to `sops`, needs no KMS permission, and cannot print a value:

```bash
navigator ops secrets apply --deployment <row> --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR" --dry-run
```

The dry run names the target project and every object, grouped by which file supplies it, and fails closed listing any
object in the Secret Manager catalog that neither file carries.

Read the *skipped* line as carefully as the failure. A skipped object is one the shared list names and this deployment
will not write, and each skip carries its reason — scoped to another deployment, integration not declared by this
deployment, or requirement satisfied another way. Those are exactly the entries `ops ship` omits from this deployment's
rendered catalog, so the line doubles as the list of what the deployment will not write. Read it to confirm the
deployment is declining what you meant it to decline. CI holds `every_projected_object_is_written_by_some_deployment`
against the synthetic tree, so no catalog entry is one that no row writes, and `ops ship` resolves every surviving
reference against live Secret Manager before it reconciles anything. Correct the tree until the dry run is clean, then:

```bash
navigator ops secrets apply --deployment <row> --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR"
```

Each object is created if absent and receives a new Secret Manager version, which becomes `versions/latest`. The same
decrypted payload is then applied as that deployment's `<name>-web-secrets` Kubernetes Secret. The CLI derives the
context, namespace, and Secret name from the selected deployment row and sends the Kubernetes manifest only on `kubectl`
standard input. Values never enter `argv`, an error message, or a log line. Run it per deployment, staging first.

Applying a value does not restart anything. `web` and `workflows-service` read the Kubernetes Secret through `envFrom`
when they start, so `navigator ops ship --deployment <name> --restart-only` forces them to it immediately.

### PostgreSQL and Forgejo service Secrets

PostgreSQL and Forgejo credentials have separate encrypted input files and use an explicit service selector. The usual
`ops secrets apply` path remains dedicated to the Navigator web/workflow environment Secret. Service files are never
read by that path and their values are never added to a web/workflow `envFrom` Secret. The service path writes only
mounted-file Secrets consumed by the selected infrastructure service; it does not deploy or restart pods.

The PostgreSQL file contains `NAVIGATOR_POSTGRES_PASSWORD`, `NAVIGATOR_POSTGRES_TLS_CERTIFICATE`, and
`NAVIGATOR_POSTGRES_TLS_PRIVATE_KEY`. It may add the client logins as a complete set of four:
`NAVIGATOR_POSTGRES_CA_CERT`, `NAVIGATOR_POSTGRES_APP_PASSWORD`, `NAVIGATOR_POSTGRES_MIGRATOR_PASSWORD`, and
`NAVIGATOR_POSTGRES_BACKUP_PASSWORD`. With them, the same apply also writes `navigator-postgres-app`,
`navigator-postgres-migrator`, and `navigator-postgres-backup`, each holding one login's `password` and the `ca.crt`
bundle; [`postgres-kubernetes.md`](postgres-kubernetes.md) names their consumers. It may also add
`NAVIGATOR_POSTGRES_DBOS_PASSWORD`, the DBOS executor's login, which the same apply writes to `navigator-postgres-dbos`
with the client set's `ca.crt`.

Forgejo's file contains exactly `NAVIGATOR_FORGEJO_DATABASE_PASSWORD`, `NAVIGATOR_FORGEJO_SECRET_KEY`,
`NAVIGATOR_FORGEJO_INTERNAL_TOKEN`, `NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET`, and `NAVIGATOR_FORGEJO_POSTGRES_CA_CERT`.
Generate the OAuth2 JWT key with `forgejo generate secret JWT_SECRET`; it must be a unique base64-encoded 32-byte value.
Mount it as a read-only file named `oauth2_jwt_secret`, read from `JWT_SECRET_URI`; the generated `app.ini` contains
only that file path. Keep PostgreSQL and Forgejo logins distinct.

Set the public `NAVIGATOR_FORGEJO_ROOT_URL` coordinate in that deployment's `config.toml` to its HTTPS URL with a
trailing `/`.

Forgejo's sign-in client at the installation's OIDC issuer is optional as a set: the encrypted
`NAVIGATOR_FORGEJO_OIDC_CLIENT_SECRET` plus the public `NAVIGATOR_FORGEJO_OIDC_CLIENT_ID` coordinate in `config.toml`. A
secret without the client id is refused. Without the pair, Forgejo has no external sign-in.

The PostgreSQL certificate must include `navigator-postgres.<namespace>.svc.cluster.local` in its SAN, as Forgejo
validates the full service DNS name.

Create each encrypted file through the deployment's own `.sops.yaml` KMS rule. The key-name-only inventory gate rejects
service credentials in the web file, a key in the wrong service file, incomplete service files, and plaintext values.
The dry run reads only those names and deployment coordinates:

```bash
navigator ops secrets apply --deployment <name> --service postgres \
  --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR" --dry-run
navigator ops secrets apply --deployment <name> --service forgejo \
  --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR" --dry-run
```

After review, apply only the service being provisioned:

```bash
navigator ops secrets apply --deployment <name> --service postgres --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR"
navigator ops secrets apply --deployment <name> --service forgejo --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR"
```

The selected command decrypts only its corresponding file in process, writes its listed values to Secret Manager, then
server-side applies only that service's mounted-file Secrets from `kubectl` stdin. PostgreSQL targets
`navigator-postgres-auth` and `navigator-postgres-tls`, plus the three client-login Secrets when the file carries them;
Forgejo targets `navigator-forgejo-config`, `navigator-forgejo-postgres-ca`, and `navigator-forgejo-database` (the
`forgejo` login's `password`, read only by the `navigator-forgejo-database` Job), plus `navigator-forgejo-oidc`
(`client_id`, `client_secret`) when the file carries the OIDC client secret. Service credentials and generated `app.ini`
are not placed in web/workflow env vars. Applying the file alone does not change an existing PostgreSQL role password or
restart a service. For the client logins, re-applying the PostgreSQL overlay re-runs the roles Job, which sets each
password from its Secret; then restart that login's clients. See the rotation steps in
[`postgres-kubernetes.md`](postgres-kubernetes.md#rotation).

## Secret Manager catalog and pod contract

`secrets/secret-provider-class.yaml` remains the per-deployment catalog for Secret Manager values. `ops ship` renders
that catalog with the selected project and namespace, removes objects the deployment intentionally skips, and verifies
each surviving `versions/latest` by metadata only. It does not decrypt, access, or write credential values.

The pod contract is separate and direct: `patches/web-env.yaml` supplies `envFrom.secretRef` for the deployment's
`<name>-web-secrets` Secret. The `ops secrets apply` harness verifies the values written to that Secret by digest, and
the rendered-manifest guard verifies the `web` container actually receives it through `envFrom`.

Before a first roll:

```bash
navigator ops secrets apply --deployment <name> --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR"
navigator ops ship --deployment <name> --tag <YY.M.D> --dry-run
```

`ops ship --dry-run` is an offline local plan: it validates the selected row and renders/parses manifest source without
cloud credentials, cluster access, or Secret Manager requests. It does not validate live Secret keys or Secret Manager
versions. A real `ops ship` checks those metadata and key-set contracts before its first mutation, never reads
credential payloads, and does not write secret material. Deploy and verify the selected deployment before proceeding to
the next one.

## Known gaps

The production deployment's `SENDGRID_API_KEY` is a **stub**, not a credential. It satisfies the boot invariant so the
parity gate is green, and it sends no mail — that deployment's outbound email is dead until a real key from the
production SendGrid account replaces it. The value is encrypted and therefore invisible in a diff, so the stub is
recorded in the production deployment's `config.toml` as well; a green gate here does not mean a working mail rail. This
is the one case where the gate reports satisfied without the deployment being usable, which is why it is written down
twice.

Objects that no deployment supplies are trimmed from the catalog rather than stubbed, because a stub is a value an
application accepts before the feature fails against. Three sets have been trimmed on those grounds: the four Workspace
Drive ids (documented in [`environments.md`](environments.md) but unconfigured; only
`NAVIGATOR_DRIVE_GCP_SERVICE_ACCOUNT_ID` exists), the three Xero ids (required only where that capability is enabled),
and `DOCUSIGN_ACCESS_TOKEN` (the alternative to the DocuSign JWT triple, which every deployment authenticates with
instead). Re-add each alongside the deployment that first carries a real value.

`NAVIGATOR_GITHUB_APP_LOGIN` is misfiled, and it is the one entry in the production deployment's `secrets.enc.yaml` that
is not key material. A bot login is a public coordinate and belongs in `config.toml`.

**Moving it to `config.toml` is the fix.** Until it moves, the exception is recorded here and in that deployment's
`config.toml`, so nobody has to rediscover why a public identifier is sitting in an encrypted file.

## Brand social accounts and runtime KMS

Facebook Pages, Instagram professional accounts, Threads profiles, and LinkedIn Pages connect through
`portal::social_oauth`; account tokens are sealed with the runtime KMS key and opened by the `workflows-service`
publisher. Both pods read the values below from the same deployment Secret.

| Key | Where | Rows |
| --- | --- | --- |
| `NAVIGATOR_RUNTIME_KMS_KEY` | `secrets.enc.yaml` | each row its own key (`independent`) |
| `NAVIGATOR_META_APP_ID` | `config.toml` coordinate | production |
| `NAVIGATOR_META_APP_SECRET` | `secrets.enc.yaml` | production only |
| `NAVIGATOR_META_WEBHOOK_VERIFY_TOKEN` | `secrets.enc.yaml` | production only |
| `NAVIGATOR_META_WEBHOOK_APP_SECRET` | `secrets.enc.yaml` | production only |
| `NAVIGATOR_THREADS_APP_ID` | `config.toml` coordinate | production |
| `NAVIGATOR_THREADS_APP_SECRET` | `secrets.enc.yaml` | production only |
| `NAVIGATOR_LINKEDIN_CLIENT_ID` | `config.toml` coordinate | production |
| `NAVIGATOR_LINKEDIN_CLIENT_SECRET` | `secrets.enc.yaml` | production only |
| `NAVIGATOR_YOUTUBE_CLIENT_ID` | `config.toml` coordinate | production |
| `NAVIGATOR_YOUTUBE_CLIENT_SECRET` | `secrets.enc.yaml` | production only |
| `NAVIGATOR_X_CLIENT_ID` | `config.toml` coordinate | production |
| `NAVIGATOR_X_CLIENT_SECRET` | `secrets.enc.yaml` | production only |
| `NAVIGATOR_TIKTOK_BUSINESS_APP_ID` | `config.toml` coordinate | production |
| `NAVIGATOR_TIKTOK_BUSINESS_APP_SECRET` | `secrets.enc.yaml` | production only |
| `NAVIGATOR_TIKTOK_BUSINESS_AUTHORIZATION_URL` | `config.toml` coordinate | production |
| `NAVIGATOR_SOCIAL_OAUTH_REDIRECT_URI` | inline env, `https://<public host>/social/oauth/callback` | every row |

The app id declares its provider. `WEB_REQUIREMENTS` then demands the provider's secret, the callback, and the runtime
KMS key; Meta also demands the webhook verify token and webhook app secret. A secret without its app id is half a
configuration and fails the same gate. A row that declares no app writes none of these objects, and `ops ship` renders
them out of its catalog. That is how staging declines social accounts: `deployments/sops-inventory.yaml` lists the
provider secrets as `production_only`, and `scripts/validate-sops-inventory.sh` refuses any of them in the staging file.
Never copy a provider secret into staging to make a staging flow work.

The runtime KMS key stands alone. A Notion database requires it; the key itself requires nothing.

Onboarding a provider is an operator step against the provider's own console, done once per row:

1. Create a dedicated runtime KMS key in the row's own project, never the deployment configuration key, and grant the
   row's runtime service account `roles/cloudkms.cryptoKeyEncrypterDecrypter` on that key alone.
2. Register the provider app with the callback above, then record the app id in `config.toml`.
3. Use `sops set --value-stdin` for each secret, never a value in shell arguments or an editor buffer that may persist.
4. Run the inventory check against the external root, then run `navigator ops secrets apply` with `--dry-run`. It lists
   the social objects supplied by encrypted secrets and deployment coordinates.
5. Apply, then `ops ship --restart-only`, and connect each Brand's accounts from its admin page.

Until those values exist, the production dry run skips every social object and `NAVIGATOR_RUNTIME_KMS_KEY` as
"integration not declared by this deployment", and the Brand admin page reports social connection as not configured.

## Secure social secret entry

Run this in your own terminal. Replace only `<production>` and the key name; the hidden prompt reads from the terminal
and sends a JSON string directly into SOPS. The secret never enters shell history, process arguments, chat, logs, or a
plaintext file. Do not enable shell tracing or terminal/session recording while entering credentials.

```bash
python3 -c 'import getpass,json; print(json.dumps(getpass.getpass("Provider secret: ")))' | \
    sops set --value-stdin deployments/<production>/secrets.enc.yaml '["NAVIGATOR_META_APP_SECRET"]'
```

Repeat for `NAVIGATOR_META_WEBHOOK_APP_SECRET` (the same Meta app secret), `NAVIGATOR_THREADS_APP_SECRET`, and, after
its app is approved, `NAVIGATOR_LINKEDIN_CLIENT_SECRET`. Choose a separate random webhook verify token; it is not an app
secret or account token. The same hidden prompt can enter it into `NAVIGATOR_META_WEBHOOK_VERIFY_TOKEN`. Keep its value
in the operator's password manager for entering the provider's webhook subscription form; never print it for inspection.

`NAVIGATOR_RUNTIME_KMS_KEY` stores the full runtime key resource name. Enter it through the same command with that key
name. It names the runtime encryption key, never the SOPS deployment configuration key. Grant the production web runtime
identity used by both pods on that key alone. Check pod service accounts and Workload Identity annotations before any
IAM change. Do not assume the service account from a different deployment is suitable.

Account access tokens are entered only through Navigator's provider OAuth consent flow. Navigator seals them in the
Brand's database record; do not put account tokens in SOPS. LinkedIn initially supports reconnection on expiry rather
than assuming eligibility for programmatic refresh.

The dry run reads names only. Apply and restart production once all entries are complete:

```bash
./scripts/validate-sops-inventory.sh
navigator ops secrets apply --deployment <production> --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR" --dry-run
navigator ops secrets apply --deployment <production> --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR"
navigator ops ship --deployment <production> --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR" --restart-only
```

Before activation, review due scheduled Posts with the operator. Restarting the production publisher with new
credentials can enable their delivery. A ready endpoint and a connected account prove configuration, not a successful
live post.

## The parity gate

`store::deployment::WEB_REQUIREMENTS` is the list of keys `web` enforces at boot, and every deployment is checked
against it in CI by `deployments::tests::every_deployment_satisfies_the_requirements_that_apply_to_it`.

The check is not "every deployment carries every key." A requirement may name one project, and the engineering webhook
five-tuple names `store::deployment::GITHUB_AUTOMATION_HOME_PROJECT` — that receiver runs in exactly one deployment and
no other may hold its credentials. Each deployment is measured against the requirements that apply to it.

A requirement may also be *triggered* by another key, which is how a deployment declines a whole integration. DocuSign
is the worked example: `DOCUSIGN_BASE_URL` declares it, and only then are the account, auth, HMAC, and webhook keys
required. A deployment that executes no documents supplies none of them and runs `StubSignatureProvider`.

Declining an integration is not the same as stubbing one, and the difference is load-bearing. A placeholder value does
not reach the stub — `portal::signature::DocuSignSignatureProvider::from_env` returns `Some` for any non-empty value, so
a fake credential boots the *real* provider and the deployment ships green and fails on its first signature request. The
stub is reachable only through genuine absence. Where an integration has no such fork, a placeholder is still the only
option: the production deployment's `SENDGRID_API_KEY` is a documented stub for exactly that reason, because
`NAVIGATOR_EMAIL_BACKEND` admits no stub backend to select.

The gate reads key names and never decrypts, so it runs with no credential and no network call. That is the whole point
of the drift bug being mechanical now: a key renamed in one place and nowhere else fails the build instead of surfacing
later as an opaque `ops ship` abort.

## See also

- [`environments`](environments.md) — the deployment matrix every coordinate here comes from.
- [`cloud-operations`](cloud-operations.md) — the operator boundary.
- [`gitops`](gitops.md) — release order and the ordered rollout.

## Owner dashboard change requests

The Owner page at `/app/owner/keys` provides preset social app identifiers and secrets. Public identifiers may be shown;
secret inputs remain blank. Saving encrypts replacements into immutable Deployment Secret Change rows with runtime KMS.
It does not grant the application access to deployment files, SOPS decryption, repository writes, or operator IAM. A
missing runtime KMS key blocks saving. The operator must bootstrap it using [Secure social secret
entry](#secure-social-secret-entry), apply SOPS, and restart before the dashboard can save.

Check configuration reads this process's loaded key presence only. It does not prove provider approval, OAuth scopes,
account identity, or publishing eligibility, and sends no post. Saved changes and loaded configuration have separate
status. Brand authorization remains on each Brand's Social Accounts page.

Register `<deployment>` in provider consoles. TikTok Business requires the trailing-slash alias `<deployment>/`. The
production coordinate is prepared in `config.toml`; a deployment is still needed to load it.

Download the encrypted change file, then run in the operator checkout with Application Default Credentials authorized
for the production runtime key and SOPS key. Export `NAVIGATOR_RUNTIME_KMS_KEY` with the production runtime key resource
name. Only the encrypted request path enters the command line:

```bash
navigator ops secrets import-social-changes --deployment <production> --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR" \
    --request <downloaded-encrypted-file>
```

The command decrypts requests in memory, sends secrets into `sops set --value-stdin`, and writes public identifiers to
`config.toml`. It refuses simulated or nonproduction targets and unknown key names. Review the resulting diff, validate
inventory, run `ops secrets apply --dry-run`, then apply and restart production when ready. It does not deploy or post.

Exports retain immutable request ids and timestamps and omit expired changes. Import rejects replacements older than 24
hours; re-enter those values through the dashboard to create a fresh request. Saved metadata remains visible until
operator reconciliation; it does not assert an applied deployment. Review the selected request timestamps and the live
configuration before importing, because an import deliberately replaces the selected SOPS values.

Firm settings link Owners to the shared social app-key editor at `/app/owner/keys`. These are deployment-scoped
production credentials, not separate provider apps for each Firm. Each Brand keeps its own account identity and
encrypted OAuth tokens. The Firm integration secret form does not configure social app keys. Admins can reach Brand
accounts, the social dashboard, and its calendar without gaining access to the Owner-only key editor.

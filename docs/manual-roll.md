# Rolling a deployment by hand

## Run without a deployment checkout

The CLI embeds the shared Kubernetes setup. Installation coordinates and secret key names can be stored in GCP Secret
Manager, so an operator needs only the CLI, an explicit project and deployment name, and cloud access:

```bash
navigator ops ship --deployment <name> --project-id <project> --tag <tag> --dry-run
navigator ops ship --deployment <name> --project-id <project> --tag <tag> --image-digests <signed-image-map>
```

The cloud record is `navigator-deployment-config-<name>` in the selected project. `--project-id` bypasses local trees
and `NAVIGATOR_DEPLOYMENTS_DIR`; it conflicts with `--deployments-dir`. The CLI refuses a record whose project or
deployment name differs from the explicit selection. It uses the same manifests, IAM, image, Secret, migration and
rollout checks as a local configuration. A live cloud ship acquires GKE credentials for the configured project, region
and cluster, then verifies the resulting context before changing workloads.

With `--project-id`, even `--dry-run` reads the configuration using Application Default Credentials. After that read,
the plan renders locally and performs no live rollout checks or writes. Without `--project-id`, dry-run uses the local
tree and needs no cloud authentication.

### The published record is the row

The record is the only source of a row. Copies of a row in a private tree, a workflow, or a shell drift; a release has
failed because three copies disagreed. Edit the record, never a copy of it.

The record stores only what cannot be derived. Every reader fills in the rest from the project id, the row name, and the
location; a value stored in the record always wins over the derived one:

| Coordinate | Derived value |
| --- | --- |
| `NAVIGATOR_GKE_CLUSTER_NAME`, `NAVIGATOR_K8S_NAMESPACE` | `<project>` |
| `NAVIGATOR_VPC_NAME`, `_SUBNETWORK_NAME`, `_GATEWAY_IP_NAME` | `<project>-vpc`, `-subnet`, `-gateway-ip` |
| `NAVIGATOR_GCP_SERVICE_ACCOUNT_ID`, `_DRIVE_GCP_SERVICE_ACCOUNT_ID` | `<project>-web`, `<project>-drive` |
| `NAVIGATOR_WEB_SECRET_NAME` | `<project>-web-secrets` |
| `NAVIGATOR_{ASSETS,DOCUMENTS,EXPORTS,LOGS,APPLICATIONS,TELEMETRY,ARCHIVES}_BUCKET` | `<project>-assets`, … |
| `NAVIGATOR_SURREAL_ARCHIVES_BUCKET` | `<project>-archives` |
| `NAVIGATOR_GKE_CONTEXT` | `gke_<project>_<location>_<cluster>` |
| KMS key | `projects/<project>/locations/<location>/keyRings/navigator-secrets/cryptoKeys/deployment-config` |

Create a new row from flags. Both capability flags are required, so no row inherits a requirement profile by accident;
`init` refuses a row that already has a record, and refuses one `ops ship` could not resolve:

```bash
navigator ops deployments init <name> --project-id <project> --location <location> \
  --github-automation-home false --sms-test-recipients false \
  --set NAVIGATOR_ENVIRONMENT=production --set NAVIGATOR_IMAGE_REGISTRY=<registry> --set ...
```

Edit it in place. Each edit reads the latest version, applies the same checks as `init`, and adds a version:

```bash
navigator ops deployments set <name> --project-id <project> KEY=VALUE ... \
  [--secret-key NAME] [--postgres-key NAME] [--forgejo-key NAME]
navigator ops deployments unset <name> --project-id <project> KEY ... \
  [--secret-key NAME] [--postgres-key NAME] [--forgejo-key NAME]
```

`unset` of a stored coordinate returns it to its derived value; the project id and location cannot be unset. The key
flags edit the secret key-name inventories, never a value. Values go straight to Secret Manager; see
[`deployment-secrets.md`](deployment-secrets.md#the-published-record).

An operator with an existing private tree imports each row once:

```bash
navigator ops deployments import <name> --deployments-dir <private-root>
```

`import` (alias `--publish <name>`) uploads coordinates and key names only. It refuses a row missing the PostgreSQL or
Forgejo service key files the ordered release requires, because that row would fail at roll time. After the import the
tree is no longer a source: make further changes with `set`, `unset`, and `ops secrets set`.

None of these verbs decrypts SOPS, uploads a credential value, applies Secrets, or rolls workloads. Cloud ships require
`secretmanager.versions.access` on the configuration object only; the edit verbs require `secretmanager.secrets.create`
and `secretmanager.versions.add`. Do not grant configuration readers access to credential objects. A missing, malformed
or unauthorized record aborts the ship without falling back to another installation or local configuration.

`ops secrets apply`, `ops observability`, `ops logs`, and `ops brand-readiness` take the same `--project-id` and read
the same record.

## Run with a private deployment checkout

A private checkout is an import source for rows not yet in the record, and a fallback while a row is migrated. Once a
row is imported, roll it with `--project-id` above; a tree edit no longer reaches the record.

`.github/workflows/release-deploy.yml` rolls every publishable tag onto the staging row, and every stable tag onto the
production row after it; see [`release-automation.md`](release-automation.md). This page is the procedure a Deployment
Operator follows to roll a row from their own machine instead: a recovery, a re-roll, a secret-only restart, a hotfix
headed for production, or any time `DEPLOY_AUTOMATION_ENABLED` is unset.

Every command runs from this checkout with `--deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR"`. Set that variable to the
approved private root containing the `deployments/` directory. Set `STAGING_ROW` and `PRODUCTION_ROW` to the directory
names supplied to you by the operator, and keep those values in the operator's local shell. `TAG` is the release tag to
roll; `ROW` is the selected deployment directory. `ROW_RESTATE_ENV` is that row's named Restate CLI environment:
`staging` for the staging row and `production` for the production row.

For the ordered GKE service overlays, `ops ship` selects the staging variant when the selected row's
`NAVIGATOR_PUBLIC_HOST` begins with `staging.` and selects production otherwise. Both variants render the selected row's
`NAVIGATOR_K8S_NAMESPACE`; the public checkout carries no deployment-specific namespace. Keep each row's host and
namespace together in the approved private configuration.

The command examples use shell variables for deployment-specific values. Populate each from the selected row's private
configuration in the operator's local shell; do not copy those values into this repository or its logs.

Download the release's signed image map before a manual full or image-only roll. It identifies every image by its
private registry reference and immutable digest; `ops ship` verifies those signatures and refuses a mismatch.

```bash
gh release download "$TAG" --repo "$GITHUB_REPOSITORY" \
  --pattern navigator-image-digests.json --dir "$CLI_DIR" --clobber
IMAGE_DIGESTS="$CLI_DIR/navigator-image-digests.json"
```

**Staging goes first.** Both rows run the same binary under the same `production` runtime profile, so a clean roll on
the staging row is the evidence that the tag boots and serves. Roll production only onto a tag staging already serves,
unless this is a rollback or an emergency where someone has explicitly weighed that and said to proceed.

Before a roll, preserve the row's intended automation and SMS-test requirement profile in its external configuration
using the explicit capability flags. The row loader does not infer these flags from project identity. Run the external
deployment preflight and dry-run with the selected root; synthetic source-tree checks do not validate operator rows.

A hand roll records no GitHub Deployment. `release-deploy.yml` reads its last-known-good tag from the `production`
Deployments it records itself, so after a hand roll of production, that record still names the tag it last shipped.

## Tools and sessions

Install `gcloud` with `gke-gcloud-auth-plugin`, `kubectl`, `sops`, `cosign`, `gh`, `restate`, `curl`, and `python3`.
`ops ship` verifies every image signature with `cosign`, and refuses with `No such file or directory` when it is absent.
`ops ship` writes its own GKE credentials and a short-lived `oauth2accesstoken` registry login for `cosign` to a scratch
directory it deletes on exit, so a `KUBECONFIG` pointing at KIND or a Docker config without a registry login does not
affect the roll. A roll rides on three sessions that expire independently, and every login is a browser or TTY flow only
the operator can run. Probe all three before starting:

```bash
case "$(uname -s)" in
    Darwin) export PATH="/opt/homebrew/share/google-cloud-sdk/bin:$PATH" ;;
esac
command -v gke-gcloud-auth-plugin >/dev/null \
  || echo "gke-gcloud-auth-plugin missing: gcloud components install gke-gcloud-auth-plugin" >&2
gcloud auth print-access-token >/dev/null                     && echo "user session OK"
gcloud auth application-default print-access-token >/dev/null && echo "ADC OK"
restate whoami 2>&1 >/dev/null | grep -q 'credentials have expired' && echo "restate DEAD" >&2
```

Run whichever login faulted. Refreshing one does nothing for the others.

```bash
gcloud auth login                                     # browser
gcloud auth application-default login                 # browser, a separate credential
restate cloud login                                   # browser, once per machine
restate cloud environments configure "$RESTATE_ACCOUNT/$ROW_RESTATE_ENV"  # TTY, once per row
```

`restate whoami` exits `0` on expired credentials and reports them only on stderr, so the grep is the test. `kubectl`
and `navigator` fall back to the user session when ADC is unavailable; `sops` reads ADC alone. A dead ADC therefore
surfaces first as an `Unauthenticated` or `invalid_rapt` decrypt error at `ops secrets apply`, which is a stale ADC and
not a missing KMS grant. Use personal, short-lived credentials, never a service-account JSON key.

The `PATH` export must be in the shell that runs `navigator`: `ops ship` shells out to `kubectl`, which reports
`executable gke-gcloud-auth-plugin not found` when the plugin is installed but off `PATH`.

## What an operator must be granted

`ops ship` reconciles the whole embedded manifest, so the grants are roles, not a short permission list:

- `roles/container.developer` on the row's project, for the reconcile.
- `roles/secretmanager.admin` on the row's project, for `ops secrets apply`. A key new to the catalog is a Secret create
  before it is a version add.
- `roles/cloudkms.cryptoKeyDecrypter` on the row's KMS key to apply secrets, or
  `roles/cloudkms.cryptoKeyEncrypterDecrypter` to author `secrets.enc.yaml`.
- `roles/iam.serviceAccountAdmin` on the row's `<row>-web` service account as a resource. The roll re-asserts that
  account's self-signing binding before it reads the tag; see
  [`gke-prod.md`](gke-prod.md#what-the-operator-credential-must-carry).
- `roles/artifactregistry.reader` on the image repository, for the image check below.

The project-level roles are bound to the deployment's configured deployer group. Read the live set, not this list. Set
the following variables from the selected external rows in the operator's local shell:

```bash
: "${NAVIGATOR_DEPLOYMENTS_DIR:?Set this to the approved private deployment root}"
: "${STAGING_ROW:?Set this to the staging row directory name}"
: "${PRODUCTION_ROW:?Set this to the production row directory name}"
: "${STAGING_PROJECT_ID:?Read this from the staging row private config}"
: "${PRODUCTION_PROJECT_ID:?Read this from the production row private config}"
: "${DEPLOYERS_GROUP:?Set the complete IAM member string, including its group: prefix}"
```

```bash
for p in "$STAGING_PROJECT_ID" "$PRODUCTION_PROJECT_ID"; do
    echo "== $p"
gcloud projects get-iam-policy "$p" --flatten=bindings --format='value(bindings.role)' \
      --filter="bindings.members:$DEPLOYERS_GROUP"
done
```

The group also holds `roles/storage.objectUser` on each row's documents bucket, so an operator can upload a workflow
input such as [`finance/<code>/input.json`](finance-aggregate.md#running-it-on-a-deployment). It is a bucket binding,
absent from the project policy above:

```bash
gcloud storage buckets get-iam-policy "$DOCUMENTS_BUCKET" --project "$PROJECT_ID"
```

`roles/container.developer` includes `container.secrets.get`, which returns values: whoever can run `ops ship` can read
every credential the row holds. Grant it only to people you would hand those credentials to.

## 1. Choose the tag

Name the tag explicitly; never derive it from the date, because prereleases (`-rc.N`, `-hotfix.N`) are routine. The
public binaries are attached to the same tag on `neon-law-source-code/homebrew-navigator`:

```bash
gh release list --repo neon-law-source-code/homebrew-navigator --limit 5
gh release view "$TAG" --repo neon-law-source-code/homebrew-navigator --json assets -q '[.assets[].name]|join(", ")'
```

Both CLI archives (macOS and Linux) must be listed. Anything less means the release run has not finished or has failed;
read that run before going further.

Record what each row serves now. The outgoing tag is the whole rollback plan:

```bash
# Select each row in turn and set its context and namespace from private config.
kubectl --context "$NAVIGATOR_GKE_CONTEXT" -n "$KUBERNETES_NAMESPACE" get deploy \
  -o jsonpath="{range .items[*]}{.metadata.name}{'\t'}{.spec.template.spec.containers[0].image}{'\n'}{end}"
```

## 2. Get the CLI at the exact tag

The GKE manifests are compiled into the binary, so a CLI from another tag applies a different tree than the images being
rolled. Download into a scratch directory outside the checkout:

```bash
CLI_DIR="$(mktemp -d)"
case "$(uname -s)" in
    Darwin) ASSET="navigator-${TAG}-macos.tar.gz" ;;
    Linux)  ASSET="navigator-${TAG}-linux.tar.gz" ;;
    *) echo "unsupported platform $(uname -s)" >&2; exit 1 ;;
esac
gh release download "$TAG" --repo neon-law-source-code/homebrew-navigator --pattern "$ASSET" --dir "$CLI_DIR"
tar -xzf "$CLI_DIR/$ASSET" -C "$CLI_DIR"
NAVIGATOR_BIN="$CLI_DIR/navigator"
"$NAVIGATOR_BIN" --version
```

The reported version must contain `TAG`.

## 3. Validate the tree

Both checks are read-only and decrypt nothing:

```bash
./scripts/validate-sops-inventory.sh
"$NAVIGATOR_BIN" ops deployments --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR"
```

Expect skips for integrations a row does not declare, such as DocuSign.

A key committed to `deployments/<row>/secrets.enc.yaml` under `$NAVIGATOR_DEPLOYMENTS_DIR` is not live until `ops
secrets apply` writes it to that row's Secret Manager, and `ops ship` refuses the roll while the two disagree. `ops
secrets apply` writes a new version of every object the row carries, so check for drift first. A Secret Manager version
newer than the tree's last change means someone wrote the provider directly, and applying would roll that value back;
reconcile the tree before applying.

```bash
# If the external root is Git-managed, inspect that repository's history.
if git -C "$NAVIGATOR_DEPLOYMENTS_DIR" rev-parse --git-dir >/dev/null 2>&1; then
  git -C "$NAVIGATOR_DEPLOYMENTS_DIR" log -1 --format='%h %ad' --date=iso -- "deployments/$ROW/secrets.enc.yaml"
else
  echo "External row tree is not Git-managed; inspect its approved version history."
fi
gcloud secrets versions list "$SECRET_NAME" --project "$PROJECT_ID" --filter='state=ENABLED' --sort-by=~createTime --limit=1
"$NAVIGATOR_BIN" ops secrets apply --deployment "$ROW" --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR" --dry-run
```

See [`deployment-secrets.md`](deployment-secrets.md#apply-secrets) for the apply itself.

## 4. Confirm the images exist

The registry deletes every image 7 days after push, so a Release on GitHub does not prove the tag can be pulled. Read
the registry out of the row so this check agrees with what `ops ship` renders:

```bash
REG=$(sed -n 's|^NAVIGATOR_IMAGE_REGISTRY = "\(.*\)"$|\1|p' "$NAVIGATOR_DEPLOYMENTS_DIR/deployments/$ROW/config.toml")
for img in "$APPLICATION_IMAGE" navigator-workflows-service navigator-archives-trigger \
           navigator-billing-digest-trigger navigator-surreal-archive \
           navigator-dri-digest-trigger navigator-general-nag navigator-nightly-project-update-trigger \
           navigator-notation-reset-trigger; do
  if gcloud artifacts docker images describe "${REG}/${img}:${TAG}" --format='value(image_summary.digest)' \
       >/dev/null 2>&1; then echo "OK    $img"; else echo "FAIL  $img"; fi
done
```

Every line must be `OK`. A `FAIL` on every line usually means the session, grant, or path is wrong: repeat the check for
the tag the row already serves. A tag older than 7 days is gone and cannot be retried; roll a newer tag or republish
from source. The same limit applies to a rollback target.

## 5. Pin the context, check grants, and review the offline plan

`ops ship --dry-run` validates the selected row and required local settings. For a full or image-only roll it also
validates the release tag and renders and parses the embedded manifests; `--restart-only` plans restarts without a
release tag. It performs no authentication, cloud or cluster reads, image checks, live Secret checks, drift diff,
Restate request, or public endpoint request. Its operation summary is a local plan only; it does not show what differs
from the live cluster. `--quiet --dry-run` keeps the summary sanitized and does not upload a transcript.

Before a real roll, `ops ship` performs the authenticated context, IAM, image, Secret Manager, live Secret, service
object, and asset checks before its first write. It fetches the row's cluster credentials itself. To check your grants
first, fetch them into your own shell:

```bash
gcloud container clusters get-credentials "$CLUSTER_NAME" --location "$GCP_LOCATION" --project "$PROJECT_ID"
kubectl config current-context   # must equal NAVIGATOR_GKE_CONTEXT in the selected external row config
for r in deployments cronjobs services secrets configmaps serviceaccounts ingresses; do
    printf '%-16s %s\n' "$r" "$(kubectl auth can-i update "$r" -n "$KUBERNETES_NAMESPACE")"
done
```

Every line must read `yes`. Do not ask about someone else with `--as`: GKE evaluates the authenticated caller's IAM, so
every impersonated answer is `no`. Read another operator's grants from `gcloud projects get-iam-policy` instead.

```bash
"$NAVIGATOR_BIN" ops ship --deployment "$ROW" --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR" --tag "$TAG" --dry-run
```

This offline plan works from the deployment tree and embedded manifests without cloud tools or credentials. It confirms
local configuration and parses manifest source, but it does not diff the live cluster or verify published image
availability, Secret Manager versions, live Secrets, IAM, assets, Restate, or public endpoints. Review the operation
summary, then perform the external image and access checks above. A live ship repeats authenticated checks before
mutation. There is no live object diff preview in this command; the plan is not evidence of drift-free or
deployment-ready state.

## 6. Ship

`RESTATE_ADMIN_TOKEN` is admin-scoped and belongs to no row, so it stays out of SOPS. Read it from the `restate` CLI's
own session immediately before `ops ship`, confirm the admin URL matches the row's recorded `RESTATE_ADMIN_URL`, and
unset both afterwards. Never print the token or write it to a file.

```bash
restate_cfg() {  # <section> <key> -> value from the restate CLI config, never echoed
  python3 - "$1" "$2" <<'PY'
import os, re, sys
section, key = sys.argv[1], sys.argv[2]
home = os.environ.get("RESTATE_CLI_CONFIG_HOME", os.path.expanduser("~/.config/restate"))
current = None
for line in open(os.path.join(home, "config.toml")):
    line = line.strip()
    match = re.match(r"^\[(.+)\]$", line)
    if match:
        current = match.group(1)
    elif current == section and "=" in line:
        name, value = line.split("=", 1)
        if name.strip() == key:
            print(value.strip().strip('"'))
            break
PY
}
RESTATE_ADMIN_TOKEN="$(restate_cfg global.cloud access_token)"
RESTATE_ADMIN_URL="$(restate_cfg "$ROW_RESTATE_ENV" admin_base_url)"
[ "$RESTATE_ADMIN_URL" = "$(sed -n 's/^RESTATE_ADMIN_URL = "\(.*\)"$/\1/p' "$NAVIGATOR_DEPLOYMENTS_DIR/deployments/$ROW/config.toml")" ] \
  || { echo "admin_base_url does not match $ROW's RESTATE_ADMIN_URL; stop" >&2; exit 1; }
export RESTATE_ADMIN_TOKEN RESTATE_ADMIN_URL
"$NAVIGATOR_BIN" ops ship --deployment "$ROW" --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR" --tag "$TAG" \
  --image-digests "$IMAGE_DIGESTS"
unset RESTATE_ADMIN_TOKEN RESTATE_ADMIN_URL
```

The session token lasts about a day; log in to Restate Cloud again if extraction fails or `ops ship` reports a Restate
auth error. Never dump the config file to inspect it. A token printed by accident is burned: log out of Restate Cloud
and log in again.

Use `--restart-only` in place of `--tag` after a secret-only change, so pods re-read `envFrom`.

A real `ops ship` can lose a late `kubectl` call to `net/http: TLS handshake timeout` after both Deployments have
rolled. A nonzero exit there is a reason to verify, not proof the roll failed: go to step 8 and read what landed before
retrying.

## 7. Restate registration

`ops ship` re-registers the worker, because Restate resolves a worker's handler list at registration and a stale list
drops webhook submissions silently. To confirm the registration is current, compare the worker's own manifest with what
Restate holds:

```bash
WF=$(sed -n 's/^NAVIGATOR_WORKFLOWS_HOST = "\(.*\)"$/\1/p' "$NAVIGATOR_DEPLOYMENTS_DIR/deployments/$ROW/config.toml")
curl -s --http2 -H "Accept: application/vnd.restate.endpointmanifest.v3+json" "https://$WF/discover"
restate -y -e "$ROW_RESTATE_ENV" services list
restate -y -e "$ROW_RESTATE_ENV" service describe "$RESTATE_SERVICE"
```

**Every `restate` command takes an explicit `-e "$ROW_RESTATE_ENV"`.** An unflagged command resolves its environment
from `$RESTATE_ENVIRONMENT` or the CLI's saved current environment, which can name the other row. A register URL names
where services point, not which environment registers them, so an unflagged `deployment register` can put one row's
services onto the other row's worker. Read `deployment register`'s confirmation output before accepting it. Before any
`--force`, confirm `restate -e "$ROW_RESTATE_ENV" deployment describe "$RESTATE_DEPLOYMENT_ID"` reports `Invocations:
0`; a nonzero count is a human decision.

## 8. Verify

```bash
./.agents/skills/deploy-staging/scripts/verify-row.sh "$ROW" "$TAG"
```

It is read-only. It checks that every Navigator image in the row's Deployments and CronJobs is on `TAG` and in the row's
registry, that pods are ready, that the public host answers `200`, that the workflows host is reachable over TLS (a
`400` is expected from that protocol endpoint), and that the GORP Serif font files serve as `font/woff2`. When the row
sets `NAVIGATOR_DBOS_ENABLED = "true"`, it also reads the current six-hour slot's `Heartbeat-<date>T<hour>` run from
`dbos.workflow_status` in a read-only `psql` session on `navigator-postgres-0`: `SUCCESS` passes, `PENDING` prints
`WAIT` (re-run in a few minutes), and a missing or failed run fails the row. Pod readiness covers PostgreSQL: `web`'s
`/app/readyz` names `postgres` when its configured database does not answer. Read every line, not only the exit status.
Then confirm the release the public host reports:

```bash
curl -fsS "https://$(sed -n 's/^NAVIGATOR_PUBLIC_HOST = "\(.*\)"$/\1/p' "$NAVIGATOR_DEPLOYMENTS_DIR/deployments/$ROW/config.toml")/version" \
  | jq .release
```

A `000` on the font check right after a roll is usually a draining load-balancer endpoint. Retry the URL a few times; if
it does not settle, `gsutil stat "gs://$ASSETS_BUCKET/fonts/gorp-serif/$FONT_FILE"` reads the bucket directly. The fonts
are operator-supplied and uploaded with `--content-type=font/woff2`.

Before blaming the tag for a symptom, run the same check against the tag the row was serving. A symptom that reproduces
on the old tag is not the tag's fault, and rolling back will not fix it.

## 9. Roll back

Roll back with `ops ship --tag "$OUTGOING_TAG" --image-digests "$OUTGOING_IMAGE_DIGESTS"`, repeating steps 4 through 8
for that tag. The registry keeps the ten newest versions per image. A `workflow_dispatch` of `release-deploy.yml` with
`rollback: true` and a `tag` production has a successful recorded Deployment for reships that tag through staging first.

## 10. Remove the kubectl context

Delete the row's kubeconfig entries once it is verified, so a later unflagged `kubectl` cannot reach it by accident:

```bash
CTX=$(sed -n 's/^NAVIGATOR_GKE_CONTEXT = "\(.*\)"$/\1/p' "$NAVIGATOR_DEPLOYMENTS_DIR/deployments/$ROW/config.toml")
kubectl config delete-context "$CTX" 2>/dev/null
kubectl config delete-cluster "$CTX" 2>/dev/null
kubectl config delete-user "$CTX" 2>/dev/null
rm -rf "$CLI_DIR"
```

Report which row is on which tag, what verified, and anything outstanding.

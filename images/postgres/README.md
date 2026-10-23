# PostgreSQL image

This directory carries the same Apache-2.0 recipe and license/notice files as
[`neon-law-source-code/neon-law-postgres`](https://github.com/neon-law-source-code/neon-law-postgres) at commit
`155a38bbec5d0cfcec0035c65b0c5c79012ab223`. Keep those three files identical to the pinned source. The directory is the
build context; the repository root has a separate license and must not replace this context.

Build from the Navigator repository root:

```sh
docker build -f images/postgres/Containerfile -t neon-law-postgres:local images/postgres
```

The recipe pins PostgreSQL 16.15, Apache AGE 1.6.0 and pgvector 0.8.2, including source checksums. PostgreSQL supplies
citext, pg_trgm and full-text search. DBOS is an application dependency and is absent from the image.

Navigator CI checks source equality, builds this recipe and runs extension, database-restart and DBOS/Axum recovery
smoke tests using the locked public test application. CI also tests the published image with the local KIND StatefulSet.
It does not publish images or deploy Navigator.

Local KIND and CI consume the published digest from `k8s/overlays/postgres-spike/kustomization.yaml`:

```sh
NAVIGATOR_KIND_DEPS_OVERLAY=k8s/overlays/kind-postgres-spike \
  navigator dev worktree-env up --runtime kind --path "$PWD"
set -a; source .devx/env; set +a
kubectl --kubeconfig .devx/kubeconfig --context "kind-$NAVIGATOR_KIND_CLUSTER" \
  -n navigator rollout status statefulset/postgres-spike --timeout=180s
kubectl --kubeconfig .devx/kubeconfig --context "kind-$NAVIGATOR_KIND_CLUSTER" \
  -n navigator port-forward service/postgres-spike :5432
```

Read the assigned loopback port from the port-forward output. This is a disposable synthetic database alongside the
existing Surreal/Restate dependencies. At handoff, remove only the task-owned tier:

```sh
navigator dev worktree-env down --path "$PWD"
```

A local rebuild need not have the published digest because build-package repositories remain mutable. Use the published
digest for shared environments. Production operator, restore, upgrade and application-migration gates remain open.

## Update the published image

1. Change the recipe in the canonical image repository. Its `Image compatibility and release` workflow tests separate
   AMD64 and ARM64 candidates, then publishes the tested manifests together without rebuilding. Record the successful
   run, source commit, and published multi-architecture digest.
2. Copy `Containerfile`, `LICENSE`, and `NOTICE` from that exact commit. Update the source commit in this README and
   `.github/workflows/postgres-compatibility.yml` together; CI compares all three files byte for byte.
3. Move the PostgreSQL digest together in the server, access, backup, restore-drill, Forgejo database, and disposable
   spike kustomizations. Update the expected digest in `.github/workflows/postgres-persistence.yml` too. The manifest
   tests enforce identical image pins.
4. Require the compatibility and persistence workflows to pass. Compatibility builds the mirrored recipe, tests the
   published digest, and checks extension and DBOS recovery. Persistence proves retained SQL and AGE data, unchanged PVC
   identity through pod replacement and StatefulSet recreation, and TLS enforcement.
5. Follow the [staging-first rollout](../../docs/postgres-kubernetes.md#postgresql-image-updates). Keep the existing
   PostgreSQL major version, retained PVC, and installed extension versions compatible with the candidate image.

Application PostgreSQL opt-in is independent of an image update. Keep the current tested digest when enabling writes
unless a demonstrated image defect requires a change. A major-version upgrade needs a separate migration procedure.

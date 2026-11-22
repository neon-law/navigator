# Container images

`images` contains every OCI Containerfile Navigator ships: the web and workflow services, scheduled workflow triggers,
the git-serving tier, and the public redirect service.

It serves release engineers and deployment operators who build one reviewed source tree into the artifacts used by KIND
and cloud environments. Keeping the definitions together makes image ownership visible and prevents each workflow from
growing an unnecessary service or bespoke build path.

All build contexts are the repository root, and the `navigator` CLI owns normal image and rollout operations. See
[GitOps](../docs/gitops.md), [cloud operations](../docs/cloud-operations.md), and the files in this directory for exact
targets.

## Site Git runtime

The Neon runtime uses a digest-pinned Debian bookworm-slim base and versioned Git, CA certificate and C++ runtime
packages. Debian supplies Git's executable, exec-path scripts and dependency closure. Governed expunge uses
`filter-branch` and a POSIX shell; copying only the Git executable does not supply that runtime. Resolved package
versions are recorded in `/usr/share/navigator/runtime-packages.txt`.

The builder compiles `repos/examples/runtime_smoke.rs` alongside Neon and the migrator in one Cargo invocation. A
build-only stage runs the real `RepoStore` initialization, attributed commits, readback, local receive-pack guards,
expunge and foreign-owner refusal as UID 65532. The final stage consumes only its proof marker and retains that same
tested runtime filesystem. It does not contain the probe, its synthetic repositories or the Rust toolchain.

This local Git check does not prove an authenticated Matter API reads its intended repository Draft. Image-level
acceptance also requires a populated synthetic repository, authorized inventory/import with matching commit and content
provenance, and continued access after restart. An empty repository or a successful readiness endpoint proves neither.

Give runtime repositories their own UID-65532-owned private directory. Group readability of a host-owned checkout does
not change Git ownership, and no global `safe.directory` override is installed. Project repositories on the forge remain
the source of truth; the site's pod-local repository directory is scratch, not a filesystem-backup guarantee.

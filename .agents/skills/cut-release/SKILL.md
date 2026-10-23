---
name: cut-release
description: Prepare a daily or explicitly named Navigator version bump for review and release through `main`.
---

# Cut a release

Read [`docs/gitops.md`](../../../docs/gitops.md), [`docs/agent-workflows.md`](../../../docs/agent-workflows.md), and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md). A release is a version-bump PR;
merging it to `main` starts publication, and `deploy.yml` creates the tag.

1. **Prepare the checkout.** Use the task's isolated worktree on a release branch based on current `origin/main`.
   Run commands from the repository root. Require a clean index and working tree so the release commit contains only the
   version bump, its lockfile, and its action pins.
2. **Select the release path.** An explicit version uses `ops release version --tag <version>` in step 4; keep that
   exact name and skip the daily probe. With no explicit version, run:

   ```bash
   cargo run -p cli --quiet -- ops cut-release --dry-run
   ```

   Capture the successful stdout as the candidate `YY.M.D`. Any nonzero exit stops the cut; read stderr for the reason.
   Exit 2 covers an already-published date and operational errors. Keep fetching enabled to compare current tags. The
   dry-run checks the daily name against tags; it does not run the browser suite or prove a write succeeds. A prerelease
   sorts below its matching ordinary release. Use the ordering in
   [`docs/gitops.md`](../../../docs/gitops.md#why-a-hotfix-prerelease-ranks-below-its-date) for a named hotfix.
3. **Run the full browser and axe-core suite.** Follow
   [`kind-local-dev`](../kind-local-dev/SKILL.md) to start the KIND fixture and host `web`. Source `.devx/env` and run:

   ```bash
   cargo run -p cli -- dev browser-e2e
   ```

   Require a passing suite with the browser and accessibility cases executing. A harness skip does not pass this gate.
   `vendor_assets` proves stylesheet rules; live axe checks require the browser suite.
4. **Write and verify the selected version.** For the daily path, rerun `ops cut-release --dry-run` after the suite.
   If its candidate differs from step 2, stop and restart the preflight with that date. Run the daily writer:

   ```bash
   cargo run -p cli --quiet -- ops cut-release
   ```

   For an explicit version, run `cargo run -p cli --quiet -- ops release version --tag <version>` instead. These
   commands write the manifest, sweep absolute Navigator action pins under `.github/` and `docs/examples/`, refresh
   `Cargo.lock`, and commit. Stop on any command failure. Inspect the commit and require the manifest version to equal
   the selected candidate; a UTC date change between the daily probe and write requires another preflight. Require a
   clean working tree and run:

   ```bash
   cargo run -p cli --quiet -- ops release pins
   cargo run -p cli --quiet -- ops release check
   cargo metadata --locked --format-version 1 > /dev/null
   ```

   Require `release check` to report a new release. Its successful already-released result does not qualify. Every
   action pin must match the selected version, including prereleases. Comments and `@YY.M.D` placeholders are excluded.
   A command failure or a mismatch stops the cut before push.
5. **Open a ready PR against `main`.** Push the version-only commit with the gate results. Apply the
   [merge gate](../../../docs/gitops.md#merge-gate-integrity-checks-with-auto-merge). Stop when the PR merges and report
   its URL, version, proof, and the outstanding fleet sweep. Publication belongs to `deploy.yml`; do not push tags,
   watch that workflow, or deploy. Keep production coordinates out of the report. Cite Linear as `ENG-1234` only.

## Fleet sweep

Run this separately when requested and after the tag is published. Use the CLI binary from that exact release. For each
authorized Project repository, dry-run the workflow and canonical-skill reconciliation, then apply it:

```bash
navigator ops github setup <owner>/<repo> --action-version <version> --dry-run
navigator ops github setup <owner>/<repo> --action-version <version>
```

This writes `ci.yml`, `cd.yml`, `navigator.yaml`'s version pin, and missing or drifted canonical Agent Skills in one PR.
A CLI from another version updates pins but cannot supply the selected release's skill bytes; rerun with the matching
binary. In each authorized Project checkout on the update branch, require the Project gate before merge:

```bash
navigator project repository sync-skills
navigator project gate --check
```

The local repair is idempotent with the matching release's API update. Preserve custom skills. The sweep is complete
when all authorized Project repositories resolve the published tag and their canonical Agent Skills match that release's
gate, including Legal Council's cited examples.

---
name: cut-release
description: Cut the next Navigator release as a version-bump PR — today's `YY.M.D`, else tomorrow's `YY.M.D-rc.N`.
---

# Cut a release

Read [`docs/gitops.md`](../../../docs/gitops.md) and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).

A release is a version-bump PR. Merging it to `main` makes `deploy.yml` build, gate, tag, and publish it. A daily
`YY.M.D` rolls through staging to production; a candidate `YY.M.D-rc.N` rolls to staging and is held there until the
next daily release.

1. **Branch.** Use an isolated worktree on a branch from current `origin/main`, with a clean tree, so the commit holds
   only the version, `Cargo.lock`, and action pins.
2. **Name it.** Unless the operator names a version, take the default:

   ```bash
   cargo run -p cli --quiet -- ops release-default-tag
   ```

   It prints today's UTC `D` while `D` is unpublished; a published `D-rc.*` does not take today's name. Once `D` is
   published, it prints `D+1-rc.N`, with `N` one past the highest published `D+1-rc.*`. A candidate ranks above every
   earlier release and below its own date; see
   [why](../../../docs/gitops.md#why-a-hotfix-prerelease-ranks-below-its-date). An empty result means a tag sits far
   ahead of the calendar: stop and report it.

3. **Run the browser and axe-core suite.** Start the KIND fixture per [`kind-local-dev`](../kind-local-dev/SKILL.md),
   source `.devx/env`, and run `cargo run -p cli -- dev browser-e2e`. A harness skip fails this gate.
4. **Write and verify.**

   ```bash
   cargo run -p cli --quiet -- ops release version --tag <version>
   cargo run -p cli --quiet -- ops release pins
   cargo run -p cli --quiet -- ops release check
   cargo metadata --locked --format-version 1 > /dev/null
   ```

   `release version` writes the manifest, sweeps action pins, refreshes `Cargo.lock`, and commits. `release check` must
   report a new release; "already released" does not qualify. Every pin must name the version. Any failure stops the
   cut before push.
5. **Open a ready PR** titled `chore(release): <version>` against `main` with the `automerge` label and the gate
   results, per the [merge gate](../../../docs/gitops.md#merge-gate-integrity-checks-with-auto-merge). Report the PR
   URL, version, and proof once it merges. Leave tagging and publishing to `deploy.yml`. Keep production coordinates out
   of the report. Cite Linear as `ENG-1234` only.

## Fleet sweep

Only when asked, after a tag is published, with the CLI from that exact release. For each authorized Project repository:

```bash
navigator ops github setup <owner>/<repo> --action-version <version> --dry-run
navigator ops github setup <owner>/<repo> --action-version <version>
```

That PR writes `ci.yml`, `cd.yml`, the `navigator.yaml` version pin, and missing or drifted canonical Agent Skills; a
CLI from another version cannot supply that release's skill bytes. On the update branch, require the Project gate before
merge, and preserve custom skills:

```bash
navigator project repository sync-skills
navigator project gate --check
```

The sweep is done when every authorized repository resolves the tag and its canonical skills pass that release's gate.

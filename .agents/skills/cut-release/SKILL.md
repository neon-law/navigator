---
name: cut-release
description: Cut a Navigator release candidate (`YY.M.D-rc.N`) as a version-bump PR. Agents never cut daily releases.
---

# Cut a release candidate

Agents cut release candidates only. The daily `YY.M.D` release is cut by the scheduled release job; never run `ops
cut-release` or bump to a plain daily version, even to ship a fix. Read [`docs/gitops.md`](../../../docs/gitops.md) and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).

A release candidate is a version-bump PR. Merging it to `main` makes `deploy.yml` build, gate, tag, and publish it, then
roll it to staging and hold it there; production waits for the daily release.

1. **Branch.** Use an isolated worktree on a branch from current `origin/main`, with a clean tree, so the commit holds
   only the version, `Cargo.lock`, and action pins.
2. **Name it.** Use `D-rc.N`: `D` is the first UTC date with no published `D` release (today, or tomorrow once today's
   has shipped), and `N` is one past the highest published `D-rc.*`. A candidate ranks above every earlier release and
   below `D` itself; see [why](../../../docs/gitops.md#why-a-hotfix-prerelease-ranks-below-its-date).

   ```bash
   gh release list --repo neon-law/navigator --limit 10
   ```

3. **Run the browser and axe-core suite.** Start the KIND fixture per [`kind-local-dev`](../kind-local-dev/SKILL.md),
   source `.devx/env`, and run `cargo run -p cli -- dev browser-e2e`. A harness skip fails this gate.
4. **Write and verify.**

   ```bash
   cargo run -p cli --quiet -- ops release version --tag <D-rc.N>
   cargo run -p cli --quiet -- ops release pins
   cargo run -p cli --quiet -- ops release check
   cargo metadata --locked --format-version 1 > /dev/null
   ```

   `release version` writes the manifest, sweeps action pins, refreshes `Cargo.lock`, and commits. `release check` must
   report a new release; "already released" does not qualify. Every pin must name the candidate. Any failure stops the
   cut before push.
5. **Open a ready PR** against `main` with the `automerge` label and the gate results, per the
   [merge gate](../../../docs/gitops.md#merge-gate-integrity-checks-with-auto-merge). Report the PR URL, version, and
   proof once it merges. Leave tagging and publishing to `deploy.yml`. Keep production coordinates out of the report.
   Cite Linear as `ENG-1234` only.

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

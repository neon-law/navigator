---
name: cut-release
description: >-
  Cut the next Navigator release as a version-bump PR: today's `YY.M.D`, else tomorrow's `YY.M.D-rc.N`. A red browser
  gate is fixed and merged in its own pull request before that bump.
---

# Cut a release

Read [`docs/gitops.md`](../../../docs/gitops.md) and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).

A release is a version-bump PR. Merging it to `main` makes `deploy.yml` build, gate, tag, and publish it. A daily
`YY.M.D` rolls through staging to production; a candidate `YY.M.D-rc.N` rolls to staging and is held there until the
next daily release. The bump commit holds only the version, `Cargo.lock`, and action pins. A failing browser gate is a
separate pull request that merges first.

1. **Branch.** Use an isolated worktree on a branch from current `origin/main`, with a clean tree.
2. **Name it.** Unless the operator names a version, take the default:

   ```bash
   cargo run -p hudson --quiet -- ops release-default-tag
   ```

   It prints today's UTC `D` while `D` is unpublished; a published `D-rc.*` does not take today's name. Once `D` is
   published, it prints `D+1-rc.N`, with `N` one past the highest published `D+1-rc.*`. A candidate ranks above every
   earlier release and below its own date; see
   [why](../../../docs/gitops.md#why-a-hotfix-prerelease-ranks-below-its-date). An empty result means a tag sits far
   ahead of the calendar: stop and report it. Naming prints a tag. It does not write the manifest.

3. **Run the browser and axe-core suite before writing the bump.** Start the KIND fixture per
   [`kind-local-dev`](../kind-local-dev/SKILL.md), source `.devx/env`, and run:

   ```bash
   cargo run -p hudson -- dev browser-e2e
   ```

   A harness skip fails this gate. `vendor_assets` is not a substitute. The command runs `browser_e2e`,
   `accessibility_e2e`, and `notation_preview_layout` on one shared store, with `--test-threads=1`, in that order. A
   green run skips the next step.

4. **When the suite fails, fix it and merge that pull request before the release pull request.** Stop the cut. Do
   not run `ops release version` on the fix branch. The release pull request stays a version bump, so the repair cannot
   ride in that commit.

   Read the failures in suite order. These suites share the seeded Lawyer and the Firm graph. A test that promotes
   `lawyer@neonlaw.com` and restores that role only after its own assertions leaves every later test signed in as the
   wrong person. The avatar check then receives 200 where it requires 404, and the inbox notes field never accepts text.
   Treat those later failures as fallout from the shared row, and re-run the whole suite after the shared row is left
   alone. Admin-only flows sign in as the seeded Admin, on a Project in that Admin's Firm. The shared Lawyer keeps the
   role `dev grant-lawyer` set. When a shared row has to change, restore it when the test fails as well as when it
   passes.

   Assert the heading the route renders. Under `main[data-matter-page-root]`, the instruments title is an `h2` and an
   undeclared page's not-found title is an `h1`. An `h1`-only check fails a page that is already correct.

   Re-run `dev browser-e2e` until all three suites pass. Open the repair as its own ready pull request, and wait until
   that commit is on `origin/main`. Fetch and rebase the release worktree onto that `main`, then run the suite once more
   on the updated tree. Only then write the version. The release commit still holds only the version, `Cargo.lock`, and
   action pins.

5. **Write and verify.**

   ```bash
   cargo run -p hudson --quiet -- ops release version --tag <version>
   cargo run -p hudson --quiet -- ops release pins
   cargo run -p hudson --quiet -- ops release check
   cargo metadata --locked --format-version 1 > /dev/null
   ```

   `release version` writes the manifest, sweeps action pins, refreshes `Cargo.lock`, and commits. `release check` must
   report a new release, not "already released". Every pin must name the version. Any failure stops the cut before push.
6. **Open a ready PR** titled `chore(release): <version>` against `main` with the `automerge` label and the gate
   results, per the [merge gate](../../../docs/gitops.md#merge-gate-integrity-checks-with-auto-merge). The diff is the
   version bump. Report the PR URL, version, and proof once it merges. Leave tagging and publishing to `deploy.yml`.
   Keep production coordinates out of the report. Cite Linear as `ENG-1234` only.

## Fleet sweep

Only when asked, after a tag is published, with the CLI from that exact release. For each authorized Project repository:

```bash
hudson ops github setup <owner>/<repo> --action-version <version> --dry-run
hudson ops github setup <owner>/<repo> --action-version <version>
```

That PR writes `ci.yml`, `cd.yml`, the `navigator.yaml` version pin, and missing or drifted canonical Agent Skills; a
CLI from another version cannot supply that release's skill bytes. On the update branch, require the Project gate before
merge, and preserve custom skills:

```bash
navigator project repository sync-skills
navigator project gate --check
```

The sweep is done when every authorized repository resolves the tag and its canonical skills pass that release's gate.

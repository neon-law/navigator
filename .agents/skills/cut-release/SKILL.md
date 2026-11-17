---
name: cut-release
description: >-
  Cut the next Navigator release as a version-bump PR: today's `YY.M.D`, else tomorrow's `YY.M.D-rc.N`. Merge a red
  browser gate in its own pull request first.
---

# Cut a release

Read [`docs/gitops.md`](../../../docs/gitops.md) and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).

A release is a version-bump PR. Merging it to `main` makes `deploy.yml` build, gate, tag, and publish. A daily `YY.M.D`
rolls through staging to production. A candidate `YY.M.D-rc.N` stays on staging until the next daily release. The bump
commit holds the version, `Cargo.lock`, and action pins. A red browser gate merges in its own pull request first.

1. **Branch.** Use an isolated worktree from current `origin/main`. Keep the tree clean.
2. **Name it.** When the operator names no version, run:

   ```bash
   cargo run -p hudson --quiet -- ops release-default-tag
   ```

   It prints today's UTC `D` while `D` is unpublished. A published `D-rc.*` leaves today's name free. Once `D` is
   published, it prints `D+1-rc.N`. `N` is one past the highest published `D+1-rc.*`. A candidate ranks above every
   earlier release and below its own date. See
   [why](../../../docs/gitops.md#why-a-hotfix-prerelease-ranks-below-its-date). Empty stdout means a tag sits far ahead
   of the calendar: stop and report it. The command prints a tag. It writes no manifest.

3. **Run the browser and axe-core suite before the bump.** Start the KIND fixture per
   [`kind-local-dev`](../kind-local-dev/SKILL.md), source `.devx/env`, and run:

   ```bash
   cargo run -p hudson -- dev browser-e2e
   ```

   A harness skip fails the gate. `vendor_assets` does not replace this run. The command runs `browser_e2e`,
   `accessibility_e2e`, and `notation_preview_layout` on one store, with `--test-threads=1`, in that order. A green run
   skips the next step.

4. **Merge a red suite in its own pull request before the release.** Stop the cut. Keep `ops release version` off
   the fix branch. The release diff stays the version bump.

   Read failures in suite order. The suites share the seeded Lawyer and the Firm graph. A test that promotes
   `lawyer@neonlaw.com`, then restores that role only after its assertions, leaves later tests signed in as the wrong
   person. The avatar check then gets 200 where it requires 404. The inbox notes field accepts no text. Treat those
   later failures as fallout. Leave the shared row alone and re-run the whole suite. Admin-only flows sign in as the
   seeded Admin, on a Project in that Admin's Firm. The shared Lawyer keeps the role `dev grant-lawyer` set. When a
   shared row must change, restore it on failure and on success.

   Assert the heading the route renders. Under `main[data-matter-page-root]`, the instruments title is an `h2`. An
   undeclared page's not-found title is an `h1`. An `h1`-only check fails a correct page.

   Re-run `dev browser-e2e` until all three suites pass. Open a ready pull request for the repair. Wait until that
   commit is on `origin/main`. Fetch, rebase the release worktree onto that `main`, and run the suite again. Then write
   the version. The release commit holds the version, `Cargo.lock`, and action pins.

5. **Write and verify.**

   ```bash
   cargo run -p hudson --quiet -- ops release version --tag <version>
   cargo run -p hudson --quiet -- ops release pins
   cargo run -p hudson --quiet -- ops release check
   cargo metadata --locked --format-version 1 > /dev/null
   ```

   `release version` writes the manifest, sweeps action pins, refreshes `Cargo.lock`, and commits. `release check`
   reports a new release, not "already released". Every pin names the version. Any failure stops the cut before push.

6. **Open a ready PR** titled `chore(release): <version>` against `main`, with the `automerge` label and the gate
   results. Follow the [merge gate](../../../docs/gitops.md#merge-gate-integrity-checks-with-auto-merge). The diff is
   the version bump. After it merges, report the PR URL, the version, and the proof. Leave tagging and publishing to
   `deploy.yml`. Keep production coordinates out of the report. Cite Linear as `ENG-1234` only.

## Fleet sweep

Run this only when asked, after the tag is published, with the CLI from that release. For each authorized Project
repository:

```bash
hudson ops github setup <owner>/<repo> --action-version <version> --dry-run
hudson ops github setup <owner>/<repo> --action-version <version>
```

That pull request writes `ci.yml`, `cd.yml`, the `navigator.yaml` version pin, and missing or drifted canonical Agent
Skills. A CLI from another version cannot supply that release's skill bytes. On the update branch, run the Project gate
before merge and keep custom skills:

```bash
navigator project repository sync-skills
navigator project gate --check
```

The sweep is done when every authorized repository resolves the tag and its canonical skills pass that release's gate.

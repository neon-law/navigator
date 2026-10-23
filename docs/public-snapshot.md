# Public snapshot

Navigator is published from a fresh root commit, not from the private repository's history. `navigator ops
public-snapshot` builds that commit: a new local Git repository whose only commit is a checked-out `main` commit's tree
minus the paths in [`.public-snapshot-exclude`](../.public-snapshot-exclude). Nothing in the private history, including
files deleted long ago, can reach the public repository through it.

The command writes only its `--out` directory. It never fetches, pushes, adds a remote, or changes a hosted repository.
Archiving the private repository and pointing a public remote at the snapshot are the manual steps below.

## What the command enforces

Before it writes anything, the command refuses (exit 2) unless:

1. The `--repo` worktree is clean: no modified, staged, or untracked files.
2. `--ref` (default `HEAD`) names the checked-out commit, so the tree being validated is the tree being published.
3. That commit is reachable from `origin/main`. Only merged history is published. The command does not fetch; run `git
   fetch origin main` first.
4. `--out` is a new or empty directory.
5. `navigator validate` passes on the worktree. The public-source audit rules run as part of it.
6. The commit carries `.public-snapshot-exclude`, and every entry is a relative path inside the repository.

## The exclusion manifest

`.public-snapshot-exclude` lives at the repository root and is read from the source commit's own tree, so the manifest
that applies is the one reviewed and merged with that commit. Each line is one repository-relative path; `#` starts a
comment. An entry excludes that file or every path under that directory. `deployments/neon-law-stg` does not exclude
`deployments/neon-law-stg-two.toml`. An entry that matches nothing produces a warning, not a refusal; remove it once the
path is gone from the tree. A test holds every checked-in entry to a tracked path.

The manifest has no entries today. Per-deployment rows live outside the repository, and the [public snapshot
audit](public-snapshot-audit.md) holds every tracked path to what may be public. Add an entry only for a tracked path
that must stay private and cannot move out of the tree.

## Determinism

The snapshot commit has no parent, a fixed message, and the source commit's committer name, email, and date as both
author and committer. Two runs on the same commit print the same SHA. `--author-name`, `--author-email`, and `--date`
replace those values; the same overrides still produce the same SHA.

`--sign` signs the commit. The signature is part of the commit, so a signed SHA differs from an unsigned rehearsal and
from every other signed run. The tree hash does not change; compare tree hashes when signing.

## Reproduce the snapshot

From a clean worktree with `main` checked out:

```bash
git fetch origin main
git switch --detach origin/main
navigator ops public-snapshot --out ../navigator-public-a
navigator ops public-snapshot --out ../navigator-public-b
```

Each run prints the snapshot commit SHA on stdout, then the snapshot tree, the source tree, and the number of excluded
paths on stderr. The two SHAs must match.

## Verify the snapshot

Run these from the source worktree. The snapshot holds exactly one commit and no parent:

```bash
git -C ../navigator-public-a rev-list --count HEAD
git -C ../navigator-public-a rev-list --max-parents=0 HEAD
```

The first prints `1`; the second prints the same SHA the command printed.

The snapshot tree differs from the source tree only by the manifest. Every remaining entry keeps its mode and blob hash,
so this diff lists only excluded paths, each as a removal:

```bash
diff <(git ls-tree -r HEAD) <(git -C ../navigator-public-a ls-tree -r HEAD)
```

The printed snapshot tree must equal the snapshot's own tree hash:

```bash
git -C ../navigator-public-a rev-parse 'HEAD^{tree}'
```

Run the public snapshot audit and the rest of the content gate on the snapshot itself, so the scanned tree is the
candidate:

```bash
navigator validate ../navigator-public-a
```

Scan the snapshot's single commit with the same scanner and configuration CI uses on every pull request:

```bash
docker run --rm --volume "$PWD/../navigator-public-a:/repo:ro" ghcr.io/gitleaks/gitleaks:v8.30.1 git --redact --config /repo/.gitleaks.toml /repo
```

## Manual checklist

These steps change hosted repositories, so they stay with the maintainer who publishes. Work through them in order.

1. Clear every row of the redistribution audit's
   [unresolved clearance blockers](licensing/redistribution-audit.md#unresolved-clearance-blockers) with evidence or a
   verified replacement. Publication waits until that evidence is complete.
2. Merge every change that must ship to `main`, wait for its `ci` check, and confirm the release workflow is not
   mid-run.
3. Reproduce the snapshot twice and verify it as above. Record the snapshot SHA and tree hash where the release is
   tracked.
4. Clone the snapshot into a new directory and follow the
   [fresh clone path](self-hosting.md#fresh-clone-to-a-running-web) from that clone. Confirm `LICENSE`, `LICENSE-MIT`,
   `LICENSE-APACHE-2`, `NOTICE`, and `THIRD-PARTY-NOTICES.txt` are present.
5. Archive the private repository: on GitHub, rename it to a private archive name, keep its visibility private, and use
   **Settings → Archive this repository**. Keep its branches, tags, pull requests, and issues; they remain the private
   record.
6. Create the new public repository under the original name, empty: no README, license, or `.gitignore` from the
   creation form.
7. In the snapshot, add the new repository as `origin` and push `main`:

   ```bash
   git -C ../navigator-public-a remote add origin git@github.com:neon-law/navigator.git
   git -C ../navigator-public-a push origin main
   ```

   If the new repository requires signed commits, push before enabling that rule, or create the snapshot with `--sign`.
8. Confirm the public repository's `main` SHA equals the recorded snapshot SHA, or its tree hash equals the recorded
   tree hash when signed.
9. Apply branch protection and repository settings with `navigator ops github setup neon-law/navigator`, then
   re-enable secret scanning and the required `ci` check.
10. Point every working checkout at the public repository: `git remote set-url origin` to the new URL, then `git fetch`.
    Old local branches share no history with the new `main`; rebase their changes onto it with `git rebase --onto`, or
    re-apply them as patches.

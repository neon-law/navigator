# GitOps: edit → merge → release → deploy

Every change reaches production through an auto-merging PR to `main`. A release is one particular such PR — the one that
bumps `[workspace.package].version` — and merging it is what creates the tag, publishes the images, and hands the
operator a deliberate rollout. This flow supports the actions in [`agent-workflows.md`](agent-workflows.md).

## `main` is sacred and squash-merge-only

- **Never commit directly to `main`.** PRs squash to one commit; merge and rebase-merge are disabled.
- **Production follows `main`.** GKE reconciles `examples/deploy/k8s/gke`, and every release tag is created by
  `deploy.yml` at a commit on `main` — so a PR branch cannot be a release source by construction, rather than by a
  check. See [`gke-prod.md`](gke-prod.md).

## The branch → PR → auto-merge flow

1. **Task worktree + branch.** Start with **New Worktree** in Codex or Claude and verify the current path is a
   non-primary `git worktree` entry. Then run `cargo run -p cli -- dev worktree-env up --branch <kebab-topic>` once. The
   CLI names that worktree's PR branch; it does not create another checkout. See
   [`agent-workflows.md`](agent-workflows.md#create-a-pr) for the stop condition when a task did not start in New
   Worktree.
2. **Push + open a PR.** `git push -u origin <branch>` then `gh pr create`.
3. **Let auto-merge land it.** `automerge.yml` enables App-owned squash auto-merge when a same-repository PR opens,
   reopens, becomes ready, or receives a push. A ready PR needs no label. Squash fires once the required gate is green
   and review threads are resolved. No approving review is required. Use draft status to hold a PR. The PR title becomes
   the Conventional Commit subject on `main`.

### Codified merge gate

Navigator is at [`neon-law/navigator`](https://github.com/neon-law/navigator) on github.com. Nothing here names a host,
sets `GH_HOST`, or repoints `NAVIGATOR_GITHUB_API_BASE`: `gh` and every Action default to the right place.

Standard GitHub-hosted runners are free for public repositories. Coordination jobs, auto-merge, CodeQL, Dependabot, the
KIND quickstart, the nightly stress test, the content gate, the CLI release archives, and the release ships use
`ubuntu-24.04`. The Rust suite, KIND integration, and the buildx image builds retain Blacksmith for its disk. The
Marketplace is available.

**Confirm an App id against a live check run before trusting it.** A required status check registered under the wrong id
still *reads* as present in the API while matching an App that never posts a check, so the gate silently enforces
nothing:

```bash
gh api repos/neon-law/navigator/commits/<sha>/check-runs \
    --jq '.check_runs[] | "\(.name) \(.app.id) \(.app.slug)"'
```

`navigator ops github setup [repository]` reconciles one repository at a time, and it governs **every** repository the
Firm administers rather than a checked-in pair. The target resolves in precedence order — the explicit `owner/name`
argument, then `GITHUB_REPOSITORY`, then the checkout's `origin` remote.

The authorization boundary is a **`(host, organization)` pair**: the source organizations — `neon-law` and
`neon-law-source-code` — and this deployment's own `NAVIGATOR_GITHUB_ORG`, on the host `NAVIGATOR_GIT_HOST` names. A
repository in none of these organizations is refused before a token is read, so an incidental checkout of someone else's
fork cannot become a write target by being the current directory. The host alone was the boundary while every repository
the Firm owned sat on a private tenant; on a public host it admits any repository on GitHub, which is why the
organization came back.

Policy is explicit, and it is not an allowlist. It forks by organization: a repository in a public organization gets
`COMMON_POLICY` and one in the deployment's own gets `CLIENT_POLICY`, which is the same gate with client-confidential
defaults — private visibility, and none of the open-source governance files a published repository carries. That gate,
in either organization, is the `production` branch protections, the CODEOWNERS assertion, and the merge policy — pull
requests only, squash only, auto-merge, automatic head-branch deletion, and squash commits titled and described from the
pull request.

GitHub `neon-law` is Navigator's public organization: Navigator, its Homebrew tap, the Zed LSP extension,
`neon-law-postgres`, and the three sample matters live there. `neon-law-source-code`, where Navigator used to live, now
holds only the published `navigator-ux`, so it keeps the public default too.

The private client-matter repositories still in `neon-law` are moving to Forgejo. Until they are gone, an interim guard
keyed on **live** visibility keeps them private in posture: a repository GitHub reports as private is reconciled with
the client-confidential publication half — no push-protection payload and no open-source governance — whatever its
organization's default. The gate half is unchanged. The guard reads visibility and never writes it.

Repository features are reconciled alongside the merge policy, in the same `PATCH`, and all three are off: Issues,
Projects, and the wiki. Issue tracking is Linear's, so a repository-level tracker is a second inbox nobody reads, and a
wiki is documentation outside the review gate every other word in the tree passes through. They were applied by hand
before this command carried them, and the hand-application did not hold, which is the argument for reconciling them
rather than trusting a setting nobody re-checks.

Secret-scanning **push protection** rides in that same `PATCH`, on a published repository only. A credential pushed to a
public tree is compromised the moment it lands and deleting the commit does not undo that — the rotation is mandatory
either way — so refusing the push is the only mitigation that happens before the disclosure. `secret_scanning` itself is
asserted alongside it: a repository made public after the fact can read scanning `disabled`, and GitHub silently drops
push protection while scanning is off. Non-provider patterns are not enabled either, deliberately; generic high-entropy
matching against legal prose and rendered PDFs blocks pushes for nothing, and a gate that fires on nothing gets bypassed
by habit.

The gate is `default_visibility` after the live-visibility guard, so a private repository omits the key from the body
entirely rather than sending it as `null`. That is not a courtesy: push protection at a private repository without
GitHub Advanced Security answers 422, so an unconditional field would fail every client-matter reconcile instead of
leaving those repositories alone.

Visibility itself is never written. A live visibility that differs from the policy's default is a dry-run finding only:
flipping it would either publish a client matter or unpublish the product.

`neon-law/navigator` alone adds `NAVIGATOR_POLICY`'s extras — the release-tag ruleset, because it is the only repository
that cuts a release, and a required `CodeQL` status check, because it is the repository that carries the CodeQL
workflow. `CodeQL` is the check the GitHub code-scanning App (id 57789) posts; it fails only on alerts a pull request
introduces, so alerts already open on `main` do not block merges. A live `production` ruleset without it is drift: `ops
github setup` reports and writes the update. The `actions/unpinned-tag` query is excluded in
`.github/codeql/codeql-config.yml` because the release cut pins this repository's own action to a tag that does not
exist yet; `cli/tests/workflow_action_pins.rs` requires every other third-party action to be pinned to a commit SHA.

There is one lighter tier and one repository in it. A repository the Firm administers on someone else's behalf still
receives the same integrity gate; what earns the exception is not ownership but whether a person writes `main` at all.
Setup verifies CODEOWNERS entries as repository content, independently of whether an approving review is required.

#### The Homebrew tap carries no gate

`neon-law/homebrew-navigator` receives `TAP_POLICY`: no `production` ruleset, no review ruleset, and no CODEOWNERS
assertion. It still receives the merge settings, which govern its occasional human pull request.

A tap is the published output of a release rather than a repository the Firm develops in. Its `main` holds one
mechanical file and grows by one commit per release, written by the tap's own `bump` workflow *after* that workflow has
computed each sha256 from the published archives, installed the formula, and run `brew test` against the binary it is
about to publish. The verification a reviewer would perform has already run, by machine, against the actual bytes — so a
review gate there has nothing left to read.

Worse, it does not merely add nothing. Every rule in the `production` ruleset refuses that write instead of governing
it: `pull_request` admits no direct push, and `required_signatures` rejects a runner's `git commit`, which GitHub
verifies only for commits made through the API or the web editor. A gated tap reports a stale version to everyone who
installed through it while every check stays green. That is not hypothetical — the ruleset cost three consecutive
releases (`26.8.21-hotfix.10`, `.11`, and `.12`), each dispatched, each dying at the bump's final `git push`, while
`brew install` served `26.8.20-hotfix.4`. `deploy.yml` now reads the formula back and fails the release when it does not
move, so a recurrence is loud rather than silent.

The assertions are off for the same structural reason rather than as a convenience: the tap has no CODEOWNERS to resolve
and no `ci` job to bind, because it has no reviewer and no test gate of that shape. Asserting either would make the
command refuse a repository it is in fact configured for.

**Adding a second exception is a policy decision, not a config change.** The test a repository must pass is that no
human writes its default branch and a machine has already proved each commit before pushing it. If you point this
command at the tap it is a no-op on rulesets — nothing is created, and because the command emits no `DeleteRuleset`,
nothing a human deliberately adds is taken away either.

#### Reconciling generated workflow content

Every Project repository's `.github/workflows/ci.yml` and `.github/workflows/cd.yml` pin Navigator's reusable gate and
publish workflows to an exact release tag — see [a repository's fixed
shell](project-repositories.md#a-repositorys-fixed-shell). Before this, moving that pin forward across the fleet after a
release meant hand-editing it in every one of the 19+ Project repositories that carry it — a manual, unreviewed in
spirit, per-repository chore.

`ops github setup` reconciles the callers against the exact templates in `cli/src/projects/repository.rs` — the same
`workflow`/`cd_workflow` generators the gate checks — and updates the top-level version in `navigator.yaml` in the same
pull request. When the selected tag matches the CLI's compiled version, it also copies missing or drifted canonical
Agent Skills into that PR, including Legal Council. It reads only the catalog's exact skill paths, preserving custom
skills. Use the target release CLI: an override for a different release updates the pins but prints a diagnostic that
the matching CLI is required for skill reconciliation. It preserves the rest of the manifest and checks whether the
repository is in scope:

- `neon-law/navigator` and the Homebrew tap carry no generated `ci.yml`/`cd.yml` in this shape, so
  this half of the reconcile is a no-op for both, same as before this feature existed.
- A deploy repository — named by the optional `NAVIGATOR_GITHUB_DEPLOY_REPO` environment variable, never a literal in
  source — carries `ci.yml`/`ship.yml`, not the Project shape, and is excluded the same way.
- Everything else this command is authorized to reconcile is verified, not assumed: it fetches `navigator.yaml` from the
  repository's root over the API. A repository the fleet's own name does not reliably identify — one live example is a
  local checkout named `vaib` for the GitHub repository `vaib-studio` — earns no exception; a repository admitted by the
  `(host, organization)` boundary that carries neither an explicit deploy-repository exemption nor a manifest fails the
  reconcile loudly, naming the repository, rather than being silently skipped or silently treated as a Project
  repository either way.

The pin defaults to this binary's own confirmed release, refusing `main`, `latest`, or an unconfirmed local build, and
accepts an `--action-version` override.

The `uses:` lines themselves default to this repository (`gate_workflow_prefix`/`publish_workflow_prefix` at
`DEFAULT_WORKFLOW_HOME`, `cli/src/projects/repository.rs`), and accept a `--workflow-home <owner>/<repo>` override. This
repository is public, so any Project repository, public or private, can call `project-gate.yml`, `project-publish.yml`,
and the composite actions they use. The override is for a fork that hosts its own copies; it sweeps the fleet the same
way an `--action-version` bump does. It is proven only against the cross-organization Project fixture in
`cli/src/devx/github_setup.rs`'s tests, so prove a live gate and publish from the fork's home before repointing real
repositories.

**A drifted file becomes a pull request, never a direct commit to `main`.** Every other reconciled artifact in this
command — a ruleset, a label, repository settings — is API-visible state with no review history of its own, so writing
the desired payload directly *is* the reconciliation. A workflow file is different: it is a tracked file on a repository
whose own ruleset requires a pull request and passing `ci` to change `main`, so writing it directly would either be
rejected by the very ruleset this command maintains or, on a repository where that ruleset is not yet applied, bypass it
outright — for a binding legal-services practice, that is not an acceptable trade for one fewer manual step. So when
`ci.yml`, `cd.yml`, or the manifest version drifts, the command opens a branch from `main` and a pull request with the
changed files. Re-running before the PR merges is idempotent: the branch is named for the exact pin, so a second run
finds the same deterministic template output and repairs any partial update before making sure the pull request is still
open, rather than stacking a duplicate.

Run a dry run before applying drift, then rerun without it:

```bash
navigator ops github setup neon-law/navigator --dry-run
navigator ops github setup neon-law/ui --dry-run
```

A second dry run after applying must report *no drift*. That is the only proof the reconcile actually converged, and it
is worth running: GitHub returns a ruleset's rules in whatever order it first stored them, which differs between a
ruleset this command created and one built by hand through REST. Comparing the rule vectors positionally made every
hand-made ruleset read as permanently drifted — each run wrote a PUT, each following run still saw drift, and "already
matches" was unreachable. `ruleset_matches` normalizes the order away, because order carries no meaning in the API. A
reconcile that never converges is a reconcile whose drift report means nothing.

#### One required check, named `ci` everywhere

Every administered repository terminates its `ci` workflow in a single aggregating job spelled exactly `ci`, and that
one context is what the ruleset requires. The job runs nothing itself: it `needs:` the real jobs and fails unless they
all succeeded. The jobs behind it stay free to differ per repository — this workspace runs `cargo test (workspace)` on a
large runner, `neon-law/ui` runs a `lint`/`verify` pair — and free to be renamed, because the required context never
moves.

The indirection exists because the alternative fails silently. A required status check is matched by string, so renaming
the job renames its check run while the ruleset goes on waiting for the old spelling. Nothing turns red; pull requests
simply sit forever on a check that will never arrive, and the usual fix — dropping the stale rule — leaves the branch
enforcing nothing at all. `ops github setup` therefore reads the repository's CI workflow and refuses to bind the gate
unless a job in it actually reports as `ci`.

It accepts either `.github/workflows/ci.yml` or `.github/workflows/gate.yml`, and looks for them in that order.
Firm-administered repositories and Project repositories both carry `ci.yml` as the required-check file. `ops github
setup` writes and reconciles that file for a confirmed Project repository. A retired `gate.yml` is still accepted so a
repository that has not been regenerated continues to bind the required `ci` check. What they share is the invariant the
gate is actually matched by — a job whose check run is named `ci` — so the filename is free to differ. A repository
carrying neither file is refused, and so is one whose workflow exists but ends in some other job name; those are
different problems with different fixes, so they are different errors.

#### Adopting a repository that is not yet governed

Host-based resolution removed the allowlist, not the convention. A repository still has to *earn* the gate, and the two
fail-closed assertions above are what it earns it with.

Every other repository the Firm administers must terminate its workflow in a job named `ci`. `assert_required_check_job`
refuses to bind the gate until that job exists. If `.github/CODEOWNERS` is absent, the command plans a one-time creation
with `* @shicholas`; if it exists, every named user or team must resolve and have write access before any ruleset is
written.

Order matters and is not a deadlock. Land the `ci` job while the ruleset still requires `verify`; both jobs run, the
pull request merges on `verify`, and the reconcile afterwards moves the required context from `verify` to `ci`. A
repository holding template content with no workflow at all has nothing for a required status check to bind to, so it
takes the CODEOWNERS half and waits for a real `ci.yml` before it can take the rest.

#### Merge gate: integrity checks with auto-merge

Every governed source repository carries `production`. It has no bypass actors and binds everyone to required checks,
signed commits, linear history, no deletion or force-push, squash-only merges, and resolved review threads. Its pull
request rule requires zero approvals, disables code-owner approval, and disables the extra-approval condition for
unattributed changes, so no approval rule can keep an otherwise-green PR from auto-merging.

Repository settings enable auto-merge. Once the required checks pass and all review threads are resolved, GitHub can
squash-merge the PR. `ops github setup` deletes an existing `production-review` ruleset on governed source repositories;
omitting it from desired state alone would leave its approval requirement active. The Homebrew tap remains outside the
branch-protection policy.

Repository permissions are the outer boundary. `ops github setup` governs branch protections only; it does not grant
repository write access. A user without write permission cannot push, regardless of the ruleset.

`required_approving_review_count: 0`, `require_code_owner_review: false`, and
`require_extra_approval_for_unattributed_changes: false` are explicit in `production`. Keeping all three conditions
clear prevents a hidden approval requirement from reappearing through bot- or agent-authored changes.

All governed repositories use the same approval-free merge gate. Required checks, resolved threads, and auto-merge
control the squash.

**Once the Project gate-pin rollout reaches a repository, its pinned `navigator project gate` runs inside the required
`ci` check.** It validates the manifest key set and value shapes, document pointers, the origin scan over the built
bundle, and the no-client-data pass. Navigator automatically arms auto-merge on ready PRs from the same repository;
required checks and thread resolution gate the merge. Drafts hold work for later, and fork PRs remain outside the
App-owned workflow.

#### CODEOWNERS owners must resolve

GitHub silently drops an unresolvable CODEOWNERS entry and leaves paths unowned. Setup verifies named `@user` and
`@org/team` entries against the API and checks that the account or team can write to the repository. This check keeps
the ownership file valid; it does not add an approval requirement to the merge gate.

This was not hypothetical while the repository sat on an EMU-provisioned enterprise that shared no account namespace
with github.com: a handle carried over from a github.com checkout resolved to nothing. The public host removed that
particular trap, leaving the general one: a misspelled handle or a person who has left the org. Both fail just as
silently. `ops github setup` resolves every account and team owner named in `.github/CODEOWNERS` against the API and
fails closed before writing anything. Email owners are retained as commit-author matches without an API lookup.

All assertions run before the first write, so a repository that cannot satisfy the policy is left exactly as it was
rather than half-reconciled.

> **Auto-merge identity.** `enable-automerge` in `automerge.yml` arms auto-merge as a GitHub App with
> `contents: write` and
> `pull_requests: write`, and as nothing else. It needs `AUTOMERGE_APP_ID` and `AUTOMERGE_APP_PRIVATE_KEY` as Actions
> secrets, **and the same pair in the Dependabot secret store**, which is separate — without them there, Dependabot's
> bumps arm nothing. There is no fallback to `GITHUB_TOKEN`: an absent secret skips arming and leaves the pull request
> visibly waiting for a human, and `cli/tests/automerge_identity.rs` asserts the fallback stays gone. Publishing sets no
> Actions *variable*; it authenticates through Workload Identity, see
> [Keyless pushes to Artifact Registry](#keyless-pushes-to-artifact-registry), and needs the one secret
> `NAVIGATOR_PUBLISHER_WIF_PROVIDER`.
>
> The App is `navigator-merge-queue` (app id `4158267`), installed on selected repositories with exactly
> `contents: write` and `pull_requests: write`, and deliberately without `workflows`. Both secrets exist, so
> auto-merge arms under that App.
>
> **Which identity arms it is load-bearing, not cosmetic.** GitHub's recursion guard names `GITHUB_TOKEN`
> specifically, so a merge armed with the run's own token produces a push that triggers no workflow at all — not a
> skipped run, not a red one: none. Because a landed version bump is the publish, such a merge loses the release
> silently, with no failing check to read. That is what happened to #95: it bumped the workspace to
> `26.8.22-hotfix.23`, `main` moved, and `deploy` never ran.
>
> The App needs permission to merge the files a PR changes. A refusal for missing `workflows` permission leaves a
> workflow-changing PR for manual handling; other merge failures fail the coordination job.
>
> `automerge.yml` calls `gh pr merge --auto --squash --match-head-commit` under the App token. The CLI arms auto-merge
> while required gates are pending and requests the governed squash merge immediately when the PR is already ready.
> The expected head commit binds both paths to the workflow event. GitHub enforces branch protections throughout.
>
> Dependabot has a separate secret store, so mirror both App secrets there. Forks do not use this path.

### TDD and the pre-commit gate

- Tests share a commit with the implementation. Rust or runtime changes require:

  ```bash
  cargo fmt
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```

- Prose-only changes require:

  ```bash
  cargo run -p cli -- project gate
  ```

- After each PR update, clean task-owned builds, KIND, browser, images, and build cache. Never prune volumes without
  approval.

## CI/CD workflows

Add jobs to the workflow that owns their trigger; do not create a redundant workflow.

The Rust dependency cache is written and read by the merge gate itself, on the branch being tested. Actions caches are
scoped per ref, so the first push of a new branch compiles the third-party graph from zero and every later push of that
branch restores it. One workflow owns the entry it reads, so there is no second file to keep in agreement.

The Actions cache is a single 10 GB budget shared by every cache in the repository, and an entry evicted before the next
push reads it is indistinguishable from one never written. That budget is why `deploy.yml`'s `build` job exports a
`type=gha` cache only on the legs carrying a `ci_cache_scope` — the scopes `publish-service` reads back. A `mode=max`
export of a Rust builder stage carries the whole `target` directory, so one leg nobody reads is enough to starve the
gate. Before adding a `cache-to` anywhere, name the reader.

Read cache health from a job log, not from the API. Every Blacksmith job logs `Enabling Blacksmith transparent cache`
and Blacksmith serves the Actions cache protocol from its own store, so `/actions/cache/usage` and `/actions/caches` can
both report zero while every `rust` run restores a full match. The `gate` job runs GitHub-hosted, so its `gate-cli`
entry lives in GitHub's own store and counts against the 10 GB quota. Grep the job for `Cache hit`, `No cache found`,
and `full match` instead — that is the only authoritative signal.

Commit-signature enforcement in CI was removed by owner decision (2026-10-30): `ci.yml`'s `gate content` job no
longer runs `navigator ops github check-signatures`. The `production` ruleset, if it still requires signed
commits in GitHub settings, is managed there and not in this repository.

| Workflow | Trigger | Job |
| --- | --- | --- |
| `.github/workflows/ci.yml` | `pull_request` → `main` | Rust quality gate |
| `.github/workflows/deploy.yml` | a push to `main`, or a `kind-ci/**` branch | prove + tag + publish, then dispatch |
| `.github/workflows/release-cut.yml` | 09:00 UTC daily | open the scheduled version-bump PR (disabled) |
| `.github/workflows/release-deploy.yml` | `workflow_dispatch` with a `tag` | ship staging, then production |

`deploy.yml`'s last job dispatches `release-deploy.yml` once every image and archive for the tag is published, while the
`DEPLOY_AUTOMATION_ENABLED` variable is `true`. Every tag rolls to staging; a stable tag then rolls to production, and a
prerelease stays on staging. `release-cut.yml` opens the version-bump PR once `RELEASE_TRAIN_ENABLED` is `true`. See
[`release-automation.md`](release-automation.md) for the gate, the identities, and rollback.

### One protection system, not two

`main` is governed by rulesets alone: `production` protects the default branch, and `release-tags` protects release
tags. The production ruleset has no bypass actors and requires status checks, signed commits, linear history, pull
requests, resolved review threads, and squash-only merging. Its approval count is zero, and no other approval rule is
desired. `ops github setup` removes an existing `production-review` ruleset so its approval requirement cannot remain
active after reconciliation.

One caveat learned the hard way: GitHub caches a pull request's merge state. Changing branch protection does **not**
recompute it for pull requests whose checks have already finished — they stay `BLOCKED` until some later event on the
pull request forces a fresh evaluation. Push a commit, or close and reopen, after changing protection.

### When `gh pr merge` refuses but the merge is legal

`gh pr merge --squash` can refuse with *"the base branch policy prohibits the merge"* on a pull request that GitHub will
merge without complaint. The CLI runs its own pre-flight against `mergeStateStatus`, and that field reads `BLOCKED`
whenever the status rollup is failing — including when every failing check is optional. The API applies the real policy
instead:

```bash
gh api -X PUT repos/neon-law-source-code/navigator/pulls/<n>/merge -f merge_method=squash
```

Confirm the required check is genuinely green first, because this bypasses the CLI's guess and nothing else:

```bash
gh pr view <n> --json statusCheckRollup \
    --jq '.statusCheckRollup[] | select(.isRequired) | "\(.name) \(.conclusion)"'
```

### PR flow — `ci.yml`

`ci.yml` runs for PRs to `main`, never pushes. The `gate` job always runs: it builds the `navigator` CLI and walks the
tree with `navigator project gate` (Markdown, YAML syntax, seed documents, locale catalogs, consumed mutable tags), then
runs Presidio and secret scans. The `rust` job runs formatting, clippy with warnings denied, and the workspace tests
with coverage. It runs only when the PR touches Rust sources or the files that job is the proof for (`.rs`, `.surql`,
`.feature`, Cargo manifests, the toolchain, nextest config, or `ci.yml` itself). A locale or Markdown edit therefore
still validates and does not spend a Blacksmith runner on the workspace suite.

The `changes`, required `ci` result, and auto-merge jobs use standard GitHub-hosted `ubuntu-24.04` runners. The
release-cut merge job and release-deploy preflight, deduplication, rollback lookup, prerelease hold, and final report
also use these runners. In `deploy.yml`, tag creation, Homebrew and Zed dispatches, success and failure reports, and
rollout dispatch also move to GitHub. These jobs run shell commands and API calls without compiling Rust or building
images.

The content gate and managed Word adapter also run on `ubuntu-24.04`: the gate compiles one debug CLI, the same build
`quickstart.yml` proves on that runner. So do the release decision, the Linux CLI archive, the CLI container and Release
publish, the daily release cut, and both release-deploy ships. The Rust suite, KIND integration, and every buildx image
build (`build`, `publish-service`, `publish-triggers`) retain `blacksmith-4vcpu-ubuntu-2404`. Public GitHub Linux
runners provide four CPUs, 16 GB RAM, and 14 GB SSD storage; private repositories get two CPUs and 8 GB RAM with the
same label. Free usage depends on actual repository visibility. See [GitHub's runner
specifications](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

The Rust workflow records a measured peak of 142 GB disk use, so CPU count alone does not make a standard runner a
suitable replacement. Its header preserves the four-versus-eight-core timing and memory measurements. Before moving any
remaining Blacksmith job, measure peak disk and memory, cold and warm cache behavior, and elapsed time on the candidate
runner. The workflow remains the source of truth for commands, caches, and pinned tool versions.

The `ci` job is the required status check — see [One required check, named `ci`
everywhere](#one-required-check-named-ci-everywhere). It aggregates `changes`, `gate`, `rust`, and `word-adapter`, and
fails unless `changes` and `gate` succeeded and the two scoped test jobs succeeded or were skipped. It tests each
dependency's result explicitly rather than relying on a bare `needs:`, because a skipped required check is not a red
one: GitHub reports no result at all, so the gate would quietly stop blocking exactly when the job it guards had failed.
A copy-only PR can skip the scoped tests and still post `ci`. The `word-adapter` job verifies its locked restore in a
Docker SDK image, then runs the .NET tests on the host.

`deploy.yml` has no `pull_request` trigger, so no PR depends on the release workflow's script and image builds. UI and
browser changes are verified locally before opening a PR — see the `web-preview` and
[`kind-local-dev`](../.agents/skills/kind-local-dev/SKILL.md) skills — and a tagged release (or a `kind-ci/**` branch
push, below) remains the CI-side KIND proof. Other path-scoped workflows check quickstart and database compatibility on
relevant pull requests.

### One workflow owns publishing — `deploy.yml`

Publishing is a deliberate act, and the act is **landing a version bump on `main`**. Bump `[workspace.package].version`
in an ordinary pull request, merge it, and that push proves the workspace in KIND, builds every image, pushes them to
Artifact Registry, creates the immutable release tag, attaches the macOS and Linux `navigator` CLI archives to that
tag's GitHub Release, hands the release to the Homebrew tap, and reports what it published.

**The version in the manifest is the release, and that is what makes a version trustworthy.** Three triggers have owned
this pipeline. A cron and a `workflow_dispatch` each derived a version from the runner clock, so the name an image
carried stood behind no Git ref — which is how `Cargo.toml` sat at `0.1.0` while published images marched on under names
the source had never heard of. A hand-pushed tag fixed the drift but paid for it in ceremony: four validations existed
purely to re-establish facts a bare ref cannot carry, and each of them, failing late, spent an immutable name.

Reading the version out of the merged manifest answers all four by construction:

| The old check | Why it is gone |
| --- | --- |
| SHAPE — a `YY.M.D` regex | `semver::Version::parse` is the shape, and it is stricter than the regex was |
| DATE — the tag equals today's UTC date | a bump merges days after it is authored, so this punished slow review |
| MANIFEST — the tag equals `[workspace.package].version` | the tag is *derived from* it; they are one decision |
| PROVENANCE — the tag's commit is reachable from `main` | a push to `main` **is** the provenance |

What survives is the one question none of them asked, and it is now the whole gate: **is this version newer than every
version already published?** `navigator ops release check` reads the manifest, lists every release tag, and compares
them with semver's own ordering. Three answers:

| Answer | What happens |
| --- | --- |
| newer than every released version | this is a release: build, prove, tag, publish |
| equal to the newest | already released — the run ends in seconds. Almost every merge |
| older than the newest | **the job fails.** A bad bump, or a rebase that resurrected an old manifest |

**A duplicate `push` for the same commit is refused before any of that runs.** `main` has, on rare occasions, been
pushed twice for one `head_sha` in the same second — GitHub's own delivery, not this repository's automation. The
`deploy-release` concurrency group below only serializes *different* commits against each other, so two runs racing for
the identical sha can both list the tags before either has tagged, and both answer "releasable" — which is how one push
built and integrated the same version twice. Before the checker or the compile, a `dedup` step lists this workflow's own
runs for `github.sha` and answers `publishable=false` when another one already exists, the same way a `kind-ci/**`
branch iteration does. A legitimate "re-run failed jobs" reuses that same run rather than creating a new one, so it is
never caught by this check.

**`release-version` runs a published `navigator`, not one it compiles.** Answering a yes/no question by building the CLI
cost ~8 minutes of latency on every merge to `main`, at the very front of the train where nothing else can start. The
job downloads the newest release that carries a Linux CLI archive, and runs `ops release check` with it. `ci.yml` still
runs the in-tree command on every pull request, so the rule is proved on the branch that changes it.

Two consequences worth stating rather than discovering. **The checker is release N-1's**, so a change to `ops release
check` itself governs from the release after the one that lands it — tolerable because the binary carries the rule while
the run supplies the data, reading this commit's manifest and the current tag list. That tolerance has one sharp edge:
**a new OUTPUT is not governed a release late, it is silently absent.** An output the tree has just started reading is
by definition one release N-1 never wrote, and an unwritten output is the empty string rather than an error — which is
how a release can skip its Homebrew hand-off. So the `checker` step does not trust a `--help` probe alone: it makes the
candidate binary write the outputs into a scratch file and falls back to a source build when any key the workflow reads
is missing. `RELEASE_OUTPUTS` on that step names the set, and `cli/tests/deploy_workflow.rs` holds it to the
`release-version` job's declared outputs so the two cannot drift. And **the binary is deliberately unpinned**, the one
exception to [Pin every consumed image, binary, and action](#pin-every-consumed-image-binary-and-action): a checker that
had to be pinned would freeze at one version and need a manual bump to ever move. `/releases/latest` is *not* how it is
found — that endpoint excludes prereleases and every release here is one, so it answers 404; the job enumerates releases
and takes the newest carrying the archive. A download that fails falls back to compiling from this tree, because a
release lost to a blipped API is the failure this pipeline has already paid for once.

The version threads into every image build as the `RELEASE_TAG` build-arg, which each Containerfile turns into the
runtime environment variable `NAVIGATOR_RELEASE_TAG`.

**`YY.M.D` is a convention, not a rule.** Nothing validates the calendar any more. The naming convention is still
`YY.M.D`, optionally suffixed with a prerelease, and the [`cut-release`](../.agents/skills/cut-release/SKILL.md) skill
is where it is written down — but a version that departs from it publishes just as well, provided it is newer than the
last one. What the date really bought was uniqueness, and comparing against the tags buys that directly.

**The default release is today's, then tomorrow's candidate.** Unless the operator names a version, the
[`cut-release`](../.agents/skills/cut-release/SKILL.md) skill cuts today's UTC `YY.M.D` while it is unpublished, and the
next date's `YY.M.D-rc.N` once it is. A published `YY.M.D-rc.N` does not take today's name. The scheduled release job
opens daily bumps only and skips a covered date.

**`ops cut-release` selects that version and writes the bump.** It compares the candidate with the release tags fetched
from `origin` and delegates the write to `ops release version`. A tag far ahead of the calendar or an operational error
exits 2. `--repo` selects the worktree for the tags, root manifest, pins, lockfile, and commit. Relative
`--manifest-path` values resolve from that worktree's root; the manifest must be its root `Cargo.toml`. Automatic
commits require a clean index and worktree on a named branch other than `main`.

`--dry-run` prints only the candidate on stdout and leaves release files unchanged. It checks tag ordering; [the
cut-release skill](../.agents/skills/cut-release/SKILL.md) supplies the browser/axe gate and verifies the selected
version, pins, and lockfile before push. An explicit version, including a hotfix, uses `ops release version --tag` and
skips the daily probe. `deploy.yml` creates the tag after merge.

**`ops release-default-tag` probes the same name.** It prints today's `YY.M.D`, or the next date's `YY.M.D-rc.N` once
today is published. A tag far ahead of the calendar produces empty stdout, a reason on stderr, and exit 0. `ops release
version --tag` writes the exact version the operator supplies.

Three shape facts still hold, because they are semver's:

- **No leading zeros.** August is `8`; `26.08.22` is not a version at all, and neither is `-hotfix.08`.
- **Three components exactly.** Cargo parses `[workspace.package].version` as strict semver, so a fourth component
  (`26.8.22.13`) cannot be written into the manifest — which is why the historical `YY.M.D.H` spelling is retired
  everywhere, including in the registry parser that once ordered it.
- **No build metadata.** `+` is not a legal character in an OCI image tag, and its precedence is not portable: the spec
  says to ignore it, the `semver` crate orders by it. `cli/src/release.rs` refuses it rather than depending on either.

**Ordering replaced the hotfix rules.** A prerelease ranks *below* its own base version (semver §11.3), so the whole "a
hotfix hangs off tomorrow's date" rule was a hand-written description of what `Version::cmp` already computes. Comparing
against the highest released version makes it a consequence instead:

```text
26.8.21  <  26.8.22-hotfix.3  <  26.8.22-hotfix.21  <  26.8.22
```

So `26.8.22-hotfix.3` is admissible after `26.8.21` and **refused** after `26.8.22`. The hyphen is exactly why — see
[Why a hotfix prerelease ranks below its date](#why-a-hotfix-prerelease-ranks-below-its-date).

**This workflow deploys nothing itself; its only cloud credential pushes images.** It ends at the registry and a
dispatch of `release-deploy.yml` — see [Publish vs. roll out](#publish-vs-roll-out).

**Run the full browser and axe-core suite before you merge the bump.** A green `ci` proves the Rust workspace. The
KIND-free `vendor_assets` test proves stylesheet rules only. Live axe WCAG failures (color-contrast on `/notations`, and
the rest of `accessibility_e2e`) appear only in `dev browser-e2e`. Bring up the KIND fixture and host `web` if they are
not running, source `.devx/env`, and run:

```bash
cargo run -p cli -- dev browser-e2e
```

A harness skip is not a pass. Do not treat `vendor_assets` as a substitute. See
[`cut-release`](../.agents/skills/cut-release/SKILL.md).

A `kind-ci/<topic>` branch push is the CI-side alternative when the change is to the workflow itself rather than to a
page — it runs `integration` alone, creates no tag, and publishes nothing.

**A failed release keeps its name.** This is the reason the tag is created *inside* the pipeline rather than pushed
ahead of it. The tag job sits between `integration` and every publisher, so:

- **A failure before the tag** — a red build, a red KIND suite — creates no ref and publishes nothing. Re-run it; the
  version keeps its name.
- **A failure after the tag** — a registry flake, a tap rejection — leaves an immutable ref, but a re-run republishes
  the same name over itself. There is nothing to un-publish, because nothing was deployed.
- **A wrong source** spends that version for good. Bump past it and merge again.

`release check` reports a version whose tag already names *this very commit* as publishable, so re-running the whole
workflow republishes rather than skipping every job and reporting success for having done nothing.

### Releasing twice in one day

Nothing forbids it, and no special spelling is required — the calendar is not a rule. Two ordinary ways to name a second
release the same day, and both are just "a bigger number":

| After releasing | A valid next version | Why |
| --- | --- | --- |
| `26.8.22` | `26.8.23` | tomorrow's date, cut early |
| `26.8.22` | `26.8.23-hotfix.1` | a prerelease of tomorrow's |

Both sort strictly above `26.8.22`, so both are admissible. What is **refused** is a prerelease of a version already
released — `26.8.22-hotfix.1` after `26.8.22` — because semver ranks it below the release it would be fixing, and every
consumer resolving those two versions would read the fix as the older one. `ops release check` says so by name rather
than letting it publish.

### Why a hotfix prerelease ranks below its date

The hyphen starts a **prerelease** identifier. `26.8.22-hotfix.1` is not "26.8.22 plus a fix"; it is an earlier,
unstable form of `26.8.22`, the same construct as `1.0.0-rc.1`. Semver §11.3 gives a prerelease lower precedence than
the matching normal version:

```text
26.8.22-hotfix.1  <  26.8.22  <  26.8.23-hotfix.1  <  26.8.23
```

After `26.8.22` is published, a same-day cut must bump the **core** — `26.8.23` or `26.8.23-hotfix.1`. A fourth numeric
component (`26.8.22.1`) is not a version Cargo can parse, and build metadata (`26.8.22+hotfix.1`) cannot name an image
tag, so neither is an escape. `ops release check` refuses the older spelling as a regression before the pipeline spends
a tag.

`N` in `-hotfix.N` is an unpadded nonnegative integer and it is the operator's to choose: a uniqueness-and-ordering
discriminator, never an hour. The padding is not cosmetic — semver forbids a leading zero in a numeric prerelease
identifier, so `hotfix.08` is not a version. Nothing derives `N`; `ops release version` writes the name it is given.

Write the version the same way as any other release, then land it:

```bash
cargo run -p cli -- ops release version --tag 26.8.23-hotfix.1
```

**A prerelease does not become the default download.** Exactly one thing behaves differently from an ordinary release,
because a prerelease must not present itself as the latest version to someone browsing the releases page:

| Surface | Ordinary release | `-hotfix.N` | other prerelease |
| --- | --- | --- | --- |
| Artifact Registry images and CLI archives | published | published | published |
| GitHub Release | latest | flagged `--prerelease` | flagged `--prerelease` |
| Homebrew tap | bumped | bumped | bumped |

The workflow holds no spelling rule for prereleases: `release check` reports them from `Version::pre`, so `-hotfix.3`
and `-rc.1` are both flagged.

**No gate in `deploy.yml` narrows below `publishable`.** The tap accepts `YY.M.D`, `YY.M.D-hotfix.N`, and `YY.M.D-rc.N`,
so the version-shape question lives in the component that acts on it. A gate that narrows below `publishable` has to
earn its place, because its failure mode is a silent skip rather than a red run: an undeclared or unwritten
`needs.<job>.outputs.<name>` evaluates to the empty string, the gate reads false, and the job skips with every job in
the run green.

**A hotfix reaches the tap, and that is load-bearing.** The formula holds exactly one version and every `brew install`
resolves to it, so the version it holds has to be the newest build that exists — not the newest build of a particular
shape. Excluding prereleases would let the formula move only when an ordinary release succeeds end to end, so a run of
ordinary releases failing at the KIND gate would leave `brew install` serving a 404 with every check green, because a
skipped job is not a failed one.

The tap owns one real constraint: **Homebrew's comparator is not semver.** It orders `26.8.20-hotfix.4` *above*
`26.8.20`, the reverse of the §11.3 ranking above. A formula walked from a prerelease to its own base version therefore
looks like a downgrade, and `brew` reports the keg as current instead of upgrading it. `scripts/bump.sh` in the tap
closes that with `version_scheme` — Homebrew's own mechanism for a version series that stops sorting forward — comparing
each new tag to the outgoing one with Homebrew's comparator and incrementing the scheme whenever the new tag does not
sort strictly above. Every bump is an upgrade, whatever the shape of either tag.

A prerelease is still a full release up to staging: it proves the workspace in KIND, publishes every image, and rolls to
staging. `release-deploy.yml` holds it there and never rolls it to production.

**The bump carries `Cargo.lock` too.** `navigator ops release version --tag <version>` writes
`[workspace.package].version` — the value every crate inherits through `version.workspace = true` and `cli/build.rs`
bakes into `navigator --version` — and refreshes `Cargo.lock` in the same commit. Every workspace crate is pinned there
as well, and the archive jobs build with `--locked`, which refuses a lock the manifest has moved past. `ci.yml` runs
`cargo metadata --locked` on every pull request for exactly that reason: it is the last place that failure is free.

`--tag` is required, because naming a release is the operator's decision and a derived name is only ever a fact about
when the command ran. That commit lands through an ordinary PR — `main` takes no direct commits.

**The release preflight is a required check now, not a habit.** On every pull request `ci.yml` runs `ops release check`,
`ops notices --check`, and the `--locked` lock check. All three lived only in the `cut-release` preflight script, run on
the operator's machine, skippable by forgetting; the merge is what publishes now, so the pull request is the last point
at which any of them is still free to fix.

`ops notices --check` was held back at first, because reading licence texts out of the runner's unpacked
`$CARGO_HOME/registry/src` made the answer a property of the machine: cargo unpacks a crate there only when something
needs it, so a build unpacks the platform it built for, and `notices_for` recorded every crate this runner had never
unpacked exactly like one that ships no licence file. Two changes fixed that. The command now refuses an unread registry
rather than folding it into the no-licence-file list, and the step runs `cargo fetch --locked` before it, which with no
`--target` unpacks every target platform's graph rather than the pair the build steps happen to need. The set it reads
is therefore `Cargo.lock`'s, the same on a macOS laptop and a Linux runner, and the `THIRD-PARTY-NOTICES.txt` shipped
inside every CLI archive names every crate rather than the ones the last build happened to need. That fetch is
load-bearing in both places it appears, the CI step and the `cut-release` preflight script, and is not tidiness to be
dropped.

**One job creates a ref, and it can only create.** `release-tag` holds `contents: write` to create `refs/tags/<version>`
at the merged commit; `release-cli-publish` holds it to create the GitHub Release against that tag. Neither can move
one: the `release-tags` ruleset restricts deletion, update, and non-fast-forward with no bypass actor, and the job
refuses a tag that already exists at a *different* commit rather than forcing it.

**The tag is created inside the same run, and that is a constraint rather than a preference.** A tag created with the
built-in `GITHUB_TOKEN` does not trigger another workflow's `on: push: tags`, so a job that created the tag and handed
off to a second workflow could not work — a separate `release-tag.yml` once cut the tag as the `navigator-release` App
purely to defeat that recursion guard. One workflow, two triggers, no App identity.

### What each stage does — `deploy.yml`

The release run proves the workspace in KIND and publishes all service and trigger images, plus two `navigator` CLI
archives attached to the GitHub Release hanging off the tag the run created: `navigator-<tag>-macos.tar.gz` and
`navigator-<tag>-linux.tar.gz`. Navigator is built for Macs and containers in the cloud; no Windows archive ships. Each
carries the executable beside `LICENSE`, `LICENSE-MIT`, and `LICENSE-APACHE-2`. Container images are **linux/amd64
only**; GKE Autopilot consumes amd64. The macOS archive is arm64 — `macos-latest` is Apple silicon — so an Intel Mac
still builds the immutable release tag locally with Cargo, and the `#navigator` report carries that exact command beside
the two downloads. Failure at any stage pages `#navigator`.

The same two build jobs also compile and attach `navigator-lsp-<tag>-<platform>` archives to the same Release, one per
platform, from the same `cargo build` invocation as the CLI. It is a distinct filename prefix (`navigator-lsp-<tag>-`
versus `navigator-<tag>-`) on the same tag, so an editor extension — a Zed marketplace listing, first — can resolve a
specific released version of the language server the way a `gh release`-based extension expects, rather than the
"latest" key `navigator ops lsp publish` writes to the public assets bucket for the site's own use. The prefix
distinguishes the assets for a consumer, not for the upload: a `navigator-*` shell glob also matches
`navigator-lsp-<tag>-*`, so every archive path in `deploy.yml` names its file tag-exactly and
[`cli/tests/release_lsp_archive.rs`](../cli/tests/release_lsp_archive.rs) refuses a wildcard there. See [editor
integration](lsp/README.md).

**Every publishing run builds both CLI archives, and Project CI depends on them.** `release-cli-build-linux` and
`release-cli-build-macos` declare the same `needs: [integration, release-version]` and the same `publishable` gate the
two publish jobs do, so they run whenever a run publishes images and skip whenever one does not (a `kind-ci/**` branch
iteration). That puts the CLI in the same stage as the Artifact Registry push rather than in a lane beside it: the
archives compile only once the KIND e2e, interop, and browser/accessibility suite is green. A release names one version
across the image tags, the two archives, `navigator --version`, and the Release page, and nobody reading that version
can tell which of those the e2e run stood behind — so all of them wait for it, and "e2e-proven" describes the whole
release instead of only the images. It also stops occupying runners on every release whose integration job then goes red
— the archives they produce have no possible consumer once the Release is never cut. Nothing reaches a stranger early
either, because `release-cli-publish` needs both publish jobs as well as the two archives — the Release is the first
fetchable artifact of the run. This is not only for human downloads: the `.github/actions/gate` composite action, the
gate **every** Project repository runs, downloads `navigator-<version>-<platform>` from this repository's public Release
(see below). If the archive jobs stop running, Project CI breaks everywhere with a download 404 and nothing in this
repository goes red — which is exactly the kind of failure worth stating in prose, because no test here will catch it.

**The Linux archive job runs on `ubuntu-24.04`; the macOS job runs on `macos-latest`.**

### The Homebrew tap

`brew install neon-law/navigator/navigator` installs the CLI, and
[`neon-law/homebrew-navigator`](https://github.com/neon-law/homebrew-navigator) is the tap it resolves. On a Mac it is
the **recommended** path, not a convenience: the released binary is unsigned and unnotarized, and Gatekeeper blocks an
unsigned Mach-O downloaded through a browser outright. Homebrew fetches with `curl`, which sets no
`com.apple.quarantine` attribute, so the same bytes run. Signing remains the right fix; the tap is what stands in until
it lands.

**ENG-115: `release-cli-build-macos` already carries the signing steps; only the Apple enrollment is missing.** The
job's `apple-signing` step checks six repository secrets — `APPLE_DEVELOPER_ID_CERTIFICATE_P12`,
`APPLE_DEVELOPER_ID_CERTIFICATE_PASSWORD`, `APPLE_DEVELOPER_ID_IDENTITY` (the exact identity string `codesign` expects,
`Developer ID Application: <name> (<team id>)`), `APPLE_NOTARIZATION_KEY_P8`, `APPLE_NOTARIZATION_KEY_ID`, and
`APPLE_NOTARIZATION_ISSUER_ID` (an App Store Connect API key scoped to notarization). Absent, as they are today, the job
logs a notice and still attaches the unsigned `navigator-<tag>-macos.tar.gz` — a missing enrollment cannot break a
release. Once an operator adds all six secrets, the same job imports the certificate into a temporary keychain,
`codesign`s the binary before it goes in the tarball, `pkgbuild`s it into `navigator-<tag>-macos.pkg`, notarizes and
staples that package, and `release-cli-publish` attaches it alongside the tarball whenever it exists. Flip
`/navigator`'s "not yet signed or notarized" copy (`neon/src/firm_copy.rs`, `server/tests/firm_routes.rs`) and the
Homebrew tap's Slack pitch (`cli/tests/homebrew_tap_dispatch.rs`) only in the release that first publishes a real
`.pkg`, not before and not as part of landing this workflow.

**Every public download resolves to this repository's own Release.** `neon-law/navigator` is public, and
`release-cli-publish` attaches the two CLI archives, the two `navigator-lsp` archives, both Linux/macOS checksums,
`install.sh`, `LICENSE`, and `NOTICE` to it. Nothing is mirrored onto the tap.
`webapp::cli_release::RELEASE_DOWNLOAD_BASE`, `cli::lsp_download::RELEASES_REPO`, `.github/actions/gate`,
`.github/actions/navigator-install`, and `.github/actions/navigator-install/install.sh`'s default all read that Release,
as does the formula's `url`. `cli/tests/public_distribution.rs` holds that contract.

`release-homebrew-tap` is the formula hand-off: it needs `release-cli-publish` and sends a `repository_dispatch` naming
the tag **and nothing else**. The tap computes every `sha256` itself by downloading the artifacts it will then tell
readers to download, from this repository's Release. A payload carrying digests would let a malformed dispatch pin the
formula to bytes nobody verified, and would leave the tap unable to repair a bad bump from a bare tag — which matters,
because `YY.M.D` admits no second ordinary release the same UTC day, so a bump that went wrong cannot be fixed by
re-cutting the release it came from. The tap covers that with a `workflow_dispatch` that re-runs any tag by hand.

**The formula does not support Intel macOS or arm64 Linux.** No prebuilt archive exists for either, and the formula does
not compile from source. It `odie`s on both with a message naming the supported alternatives, and the tap's
`unsupported` CI job proves that refusal weekly.

**A separate repository, not a folder here.** A tap is a Git repository Homebrew clones and re-reads on every `brew
update`, and its formula changes once per release, mechanically, with no review to add. Keeping it here would mean
either a bot commit to a protected `main` or a PR nobody reads, and would put a full workspace clone in front of every
`brew update`.

The dispatch authenticates with `HOMEBREW_TAP_TOKEN`, a fine-grained token scoped to `contents: write` on the tap and
nothing else — the run's own `GITHUB_TOKEN` cannot write to another repository, and widening it to one that could would
hand that reach to every job in the workflow. **A missing or rejected token fails the release**, deliberately: a tap
that silently stops updating serves a stale version to everyone who installed through it while nothing anywhere goes
red, which is the Project-CI 404 one channel over. `cli/tests/homebrew_tap_dispatch.rs` holds the contract, because the
two repositories never reference each other.

**The release then waits for the formula, and asks the tap's own run why it has not moved.** An accepted dispatch is not
a completed bump: `POST /dispatches` answers 204 the moment GitHub queues the event and carries no run id, and three
consecutive releases were told, all three bumps died, and all three stayed green here. So the job reads the formula back
and fails unless it names this tag — **the formula is the proof, because a bump that goes green without committing is
still a stale tap.**

What the formula alone cannot say is *why* it has not moved. A bump that died thirty seconds ago and a bump still queued
read identically as "not this tag yet", so a wait that can only see the formula spends its whole budget on a bump that
already failed — which is how a 27-second answer cost 99 minutes. The job therefore also reads the tap's `bump` runs,
narrowed to `event=repository_dispatch` and to runs created since this release dispatched, and **fails the moment that
run's conclusion is anything but success.** The conclusion may only ever end the wait early: it never stands in for the
formula, and a read that fails leaves the loop exactly the formula poll it was, bounded by the same ceiling.

Reading those runs widens nothing. **The tap is a public repository**, so its run list is public data and the run's own
`GITHUB_TOKEN` reads it; `HOMEBREW_TAP_TOKEN` stays `contents: write` and never acquires `actions: read`. The tap's
`workflow_dispatch` repair path is deliberately not matched, because a human re-running a bump by hand is answered by
the formula check, which runs first on every pass.

Two platforms have no prebuilt archive, Intel macOS and arm64 Linux, and the formula compiles no source-tag fallback for
them (see above). The tap's own CI gates every push on the two prebuilt platforms and weekly proves the other two refuse
to install rather than silently attempting a build.

The run narrates itself while it goes when `SLACK_WEBHOOK_URL` is configured. Every forward-path step opens with a
`.github/actions/slack-progress` post to `#navigator` naming the tag, the stage, and the step, so the channel watches
the release advance rather than waiting ~45 minutes for a verdict — and the last line posted names the step a failure
died in, before anyone opens the run. All Slack reporting is advisory: an absent webhook notices and skips, and a
rejected post warns, because a release must not be lost to its own narration. GitHub's run conclusion remains the source
of truth. The posts self-gate on the trigger ref the same way the two terminal reports do, so a `kind-ci/**` branch
iteration stays silent. Steps gated on `failure()` or `always()` are post-mortem diagnostics rather than progress and
are deliberately not narrated; the failure page already covers that moment when Slack is configured.
`cli/tests/deploy_slack_progress.rs` holds the narration complete — a new step added without a post fails that gate,
because nobody notices a *missing* Slack line.

### The Zed extension

[`zed-navigator-lsp`](https://github.com/neon-law/zed-navigator-lsp) is the dev extension that attaches `navigator-lsp`
to Markdown in Zed. `release-zed-extension` hands it each release the way `release-homebrew-tap` hands the tap one: the
same `navigator-release` dispatch, naming the tag and nothing else, once the Release carries its archives. The
extension's `bump.yml` sets its version to the tag, builds it, and pushes `v<tag>`, so the two repositories name one
version, and the release waits for that tag.

The dispatch authenticates with `ZED_EXTENSION_TOKEN`, a fine-grained token scoped to `contents: write` on the extension
and nothing else, and a missing token fails the release as the tap's does. It carries the version only: the extension
resolves `navigator-lsp` from this repository's Releases at runtime, and nothing here publishes to Zed's extension
registry. [`cli/tests/zed_extension_dispatch.rs`](../cli/tests/zed_extension_dispatch.rs) holds the contract.

### What detects a broken pipeline

**Nothing on a clock does.** The nightly train doubled as a daily liveness check on the whole release path: images still
build, KIND still stands up, `ops ship` still authenticates. A defect in the publishing stages is invisible until
someone next bumps the version. And the cron was never a reliable signal even while it ran — a silent nightly failure
went unnoticed for four consecutive nights — so the honest statement is that this pipeline has no automatic breakage
detection, not that it has a weaker one.

One part of it did get cheaper. `deploy.yml`'s first job now runs on **every** merge to `main`, so a break in the
release trigger itself — the manifest read, the tag comparison, the guard binary — surfaces at the next merge rather
than at the next release. The build, KIND, tag, and publish stages still wait for a real bump.

What remains, and what each does not cover:

- When `SLACK_WEBHOOK_URL` is configured, `notify-failure` pages `#navigator` when a release fails, reading the trigger
  ref rather than a job output, so a failure anywhere — including the release decision — still pages. Reading the ref
  costs nothing in false positives: a merge carrying no bump has nothing to fail, because the decision job succeeds,
  reports `publishable=false`, and every other job skips. It can only fire on a run that happened; without the optional
  webhook, the GitHub run conclusion is the signal.
- `kind-ci/**` proves a release-workflow change on demand: push a `kind-ci/<topic>` branch to run the KIND integration
  job alone, creating no tag, publishing nothing and shipping nothing. On demand, not on a schedule.
- `ci.yml` always validates YAML and Markdown. It proves the Rust workspace, plus the three release preflight checks —
  `ops release check`, `ops notices --check`, and `cargo metadata --locked` — when the PR touches Rust. It still says
  nothing about images, KIND, or shipping.

Two consequences to plan around rather than discover:

- A `kind-ci/**` push is the cheapest way to exercise the pipeline without releasing. No cron runs it, so run it as a
  habit. - **Image expiry does not depend on release cadence, and that cuts the other way**: the registry deletes every
  image 7 days after push, so a quiet week leaves no pullable release. See [Keyless pushes to Artifact
  Registry](#keyless-pushes-to-artifact-registry).

### Recovering a failed release

Three lanes, cheapest first.

1. **Re-run the failed jobs** from the run's page, or dispatch `deploy.yml` again. The version derives from the same
   UTC day either way, so a re-run republishes that same name over itself. This is the move for a flake: a runner disk,
   a registry timeout, a wedged port-forward. Nothing was deployed, so there is nothing to un-deploy. A re-run replays
   the run's own commit and cannot pick up a fix merged after the tag; recovery is `gh release create <tag>` plus the
   run's artifacts, or a deliberate skip.
2. **`ops ship` the already-published tag.** If the images published green and only a roll failed, rebuild nothing:

   ```bash
   gh release download YY.M.D --pattern navigator-image-digests.json --dir /tmp
   navigator ops ship --deployment <row> --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR" --tag YY.M.D \
     --image-digests /tmp/navigator-image-digests.json
   ```

   This is [The manual deploy](#the-manual-deploy). `ship` builds nothing, refuses a tag absent from the registry, and
   `--dry-run` rehearses it first.
3. **Fix forward the same day with a `-hotfix.N` tag.** If the source is wrong, land the fix on `main`, bump it with
   `ops release version`, passing the `-hotfix.N` name you chose as `--tag`, and tag the merged commit. See [Releasing
   twice in one day](#releasing-twice-in-one-day) for the shape and for the two surfaces a hotfix deliberately does not
   touch. A hotfix is a prerelease, so `release-deploy.yml` rolls it to staging and holds it there; production takes the
   hotfix through [The manual deploy](#the-manual-deploy) once staging serves it.

**The day's `YY.M.D` name is still spent, and the tag still cannot move.** The `release-tags` ruleset restricts
deletion, update, and non-fast-forward with no bypass actor, and no second `YY.M.D` exists for that day — so a same-day
fix takes a *new* name rather than a moved one. Deleting and re-pushing a tag is never the answer: a moved tag makes
every artifact already carrying that version a lie. Rolling back with `ops ship --tag <previous>` remains the right move
while a fix is still being written, since a hotfix has to be proven before it is worth shipping.

### Keyless pushes to Artifact Registry

The publish jobs hold no registry key and no PAT. Every image goes to `<deployment>`, and the login is a short-lived
access token minted by exchanging the run's GitHub OIDC token through Workload Identity Federation:

```yaml
- uses: google-github-actions/auth@7c6bc770dae815cd3e89ee6cdf493a5fab2cc093 # v3
  with:
    workload_identity_provider: ${{ secrets.NAVIGATOR_PUBLISHER_WIF_PROVIDER }}
    service_account: service-account@example.invalid
    token_format: access_token
- uses: docker/login-action@dbcb813823bdd20940b903addbd779551569679f # v4.6.0
  with:
    registry: <deployment>-docker.pkg.dev
    username: oauth2accesstoken
    password: ${{ steps.gcp.outputs.access_token }}
```

**Third-party actions are pinned to a full commit SHA**, with the version in a trailing comment, in every workflow that
holds a credential. A tag is mutable, so a hijacked `v3` would run with the job's OIDC token and registry access; a SHA
cannot move. Dependabot's `github-actions` ecosystem bumps the SHA and the comment together.

**The identity is one service account and one provider.** `navigator-publisher` lives in the `<registry-project>`
project and holds `roles/artifactregistry.writer` on the `navigator` repository only. The Workload Identity provider
trusts this GitHub repository alone (`assertion.repository`), and only that repository may impersonate the service
account. The provider resource name carries the project number, so it is stored as the secret
`NAVIGATOR_PUBLISHER_WIF_PROVIDER`; it names no credential. If it is unset the exchange fails the job, so it is not a
toggle.

These commands create all of it:

```bash
PROJECT=<registry-project>
GITHUB_REPO=neon-law/navigator
SA=navigator-publisher

gcloud services enable artifactregistry.googleapis.com iamcredentials.googleapis.com --project "$PROJECT"

gcloud iam service-accounts create "$SA" --project "$PROJECT" \
  --display-name "Navigator image publisher"

gcloud artifacts repositories add-iam-policy-binding navigator \
  --project "$PROJECT" --location <deployment> \
  --member "serviceAccount:$SA@$PROJECT.iam.gserviceaccount.com" \
  --role roles/artifactregistry.writer

gcloud iam workload-identity-pools create navigator-github \
  --project "$PROJECT" --location global --display-name "Navigator GitHub"

gcloud iam workload-identity-pools providers create-oidc navigator-publisher \
  --project "$PROJECT" --location global --workload-identity-pool navigator-github \
  --issuer-uri "https://token.actions.githubusercontent.com" \
  --attribute-mapping "google.subject=assertion.sub,attribute.repository=assertion.repository" \
  --attribute-condition "assertion.repository == '$GITHUB_REPO'"

PROJECT_NUMBER=$(gcloud projects describe "$PROJECT" --format 'value(projectNumber)')
gcloud iam service-accounts add-iam-policy-binding \
  "$SA@$PROJECT.iam.gserviceaccount.com" --project "$PROJECT" \
  --role roles/iam.workloadIdentityUser \
  --member "principalSet://iam.googleapis.com/projects/$PROJECT_NUMBER/locations/global/workloadIdentityPools/navigator-github/attribute.repository/$GITHUB_REPO"

gh secret set NAVIGATOR_PUBLISHER_WIF_PROVIDER --repo "$GITHUB_REPO" --body \
  "projects/$PROJECT_NUMBER/locations/global/workloadIdentityPools/navigator-github/providers/navigator-publisher"
```

**`iamcredentials` is the step that is easy to miss.** Every earlier step succeeds without it, and so does the OIDC
exchange. The job then fails at `authenticate to Artifact Registry` with `403 SERVICE_DISABLED`: *IAM Service Account
Credentials API has not been used in project …*. The 26.9.30 release failed this way. That error means an API is off,
not a missing IAM binding. Enable the API, then run `gh run rerun <run-id> --failed`.

**The grant is per job, not workflow-wide.** The top-level `permissions:` block holds `contents: read`; only
`publish-service` and `publish-triggers` add `id-token: write`, so no other job in the release can mint a cloud
credential. `cli/tests/deploy_workflow.rs::deploy_workflow_ships_nothing_and_federates_only_to_push_images` asserts that
only those two jobs federate, and only into the publisher.

**A fork changes one variable, not three.** `cli::devx::registry::DEFAULT_REGISTRY` is `<deployment>` and
`NAVIGATOR_IMAGE_REGISTRY` overrides it. `cli/tests/deploy_workflow.rs::the_workflow_pushes_where_the_cli_pulls` holds
the workflow and the constant to one string.

**Artifact Registry keeps ten versions per image.** `ops gcp hub setup` applies the count-based keep-last-10 cleanup
policy, so each image retains its ten newest published versions regardless of how frequently releases happen. This is
the rollback window; a release older than those ten versions may be removed. The `:latest` alias points to the same
release digest and does not consume an additional slot; the separate build-cache image is subject to the same count.

Release jobs record each buildx OCI index digest and sign private refs with keyless cosign. They attach a
repository-qualified digest map to the GitHub Release and pass it to `release-deploy.yml`. GHCR gets a best-effort
mirror of those digests; a GHCR outage cannot block the private publish or deployment. `ops ship` checks each digest
against its release tag, verifies the GitHub Actions OIDC identity, then applies `@sha256:` manifests. The Navigator CLI
image comes from the already-built Linux archive, so deploy runners extract it by digest.

**The image-hub commands are separate.** `ops gcp hub setup` provisions its own repository and a `navigator-ci-pusher`
service account in a hub project. Nothing in the publish path uses it: the `<registry-project>` repository above is what
CI writes and what `ops ship` reads.

## Pin every consumed image, binary, and action

**Every consumed image, binary, and action is immutable.** Publishing `latest` is allowed; consuming it is not.

Embedded Rego policy is load-bearing: `cli/tests/regorus_policy.rs` compiles the production source and runs every
checked-in policy rule. The Regorus version and policy source ship together in the web binary; upgrading either is a
deliberate, tested change.

- **Images** (`image:`, `FROM`): pin an explicit version tag, never `latest` or another rolling tag, and confirm the tag
  still exists on the registry we pull from before pinning.
- **Installer binaries** (a workflow step's `version:`): pin the version, never `latest` — `latest` also round-trips a
  release API that has 500'd and killed a job. The one exception is `release-version`'s own checker, which must follow
  the newest release to be useful at all; see [One workflow owns publishing](#one-workflow-owns-publishing--deployyml).
- **Third-party GitHub Actions** (`uses:`): pin the full commit SHA with a trailing `# vX.Y.Z` comment, per GitHub's
  guidance — a bare `@v2` resolves to a branch tip upstream can force-push.

`navigator project gate` rejects mutable consumption under `k8s/`, `examples/`, `images/`, and `.github/workflows/`;
`deploy.yml` publication sites are exempt.

## Publish vs. roll out

`deploy.yml` publishes; `release-deploy.yml` rolls. The publish jobs federate only into the registry publisher, and no
job in `deploy.yml` can mutate a cluster. `cli/tests/deploy_workflow.rs` asserts both — no job named `ship*`, and only
the two publish jobs exchange a credential.

Every roll target is a directory in this repository's `deployments/` tree: `<deployment>` and `<deployment>`, each a
`config.toml` of plaintext coordinates and a `secrets.enc.yaml` of SOPS ciphertext. `ci.yml`'s `gate` job runs Run
`scripts/validate-sops-inventory.sh` and `navigator ops deployments` against the external tree before each pull request;
both read key names only.

Staging is the gate on the way to production. It runs the same `<deployment>` image over a sample data plane, so a
failure there is evidence about the version rather than about real people's matters.

### The deploy pipeline

1. `deploy.yml` publishes every image and attaches every CLI and LSP archive to this repository's Release.
2. Its `release-deploy` job needs every one of those jobs. When `DEPLOY_AUTOMATION_ENABLED` is `true`, it runs
   `gh workflow run release-deploy.yml --ref main -f tag=<tag> \
   -f image-digests=<release-map-json>` with `actions: write`; otherwise it dispatches nothing.
3. `release-deploy.yml` verifies the Release, rolls `<deployment>` with the full `ops ship`, and runs its health checks.
   Each check posts its result to `#navigator`.
4. A prerelease stops there, and `#navigator` hears that the tag was held at staging. A stable tag posts that staging
   passed and rolls `<deployment>` the same way, recording a GitHub Deployment against `production`.

`notify` posts what was published, the CLI installation instructions, and whether the rollout was dispatched, with the
manual `ops ship` command beside it. Row names in that message are placeholders; every literal `--deployment <name>` in
a workflow or document must name a real row, which `every_instructed_deployment_names_a_real_row` in
`cli/src/devx/deployments.rs` enforces.

### The manual deploy

A re-roll, a rehearsal, or a recovery outside the pipeline is one `ops ship` per row, run from this checkout with your
own short-lived credentials, staging first:

```bash
gcloud auth application-default login
gh release download YY.M.D --pattern navigator-image-digests.json --dir /tmp
navigator ops ship --deployment <row> --deployments-dir "$NAVIGATOR_DEPLOYMENTS_DIR" --tag YY.M.D \
  --image-digests /tmp/navigator-image-digests.json
```

`--deployment` is required and reads every coordinate from `deployments/<name>/config.toml` — never from the shell, so a
stale environment cannot select the wrong deployment. `ship` builds nothing. Before a full or image-only roll it checks
every embedded slide-media key directly in that deployment's `NAVIGATOR_ASSETS_BUCKET`. It then validates the secrets,
reconciles manifests, rolls every service and trigger to one tag, and re-registers Restate. After a secret rotation, a
`--restart-only` ship restarts the pods without changing the version; that lane is manual only. See
[`manual-roll.md`](manual-roll.md), [`cloud-operations.md`](cloud-operations.md), and
[`gke-prod.md`](gke-prod.md#trust-boundary).

**Rolling back is the same command with an older version**, or a `release-deploy.yml` dispatch with `rollback: true` and
a tag production has served. Either way the images must still exist, and they are deleted 7 days after push — see
[Keyless pushes to Artifact Registry](#keyless-pushes-to-artifact-registry).

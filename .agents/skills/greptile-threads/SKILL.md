---
name: greptile-threads
description: >
  Bring every Greptile review thread on an open pull request up to date: list the unresolved threads, settle each one
  (fix it through review-pr, or decline it with evidence), then reply and resolve it so auto-merge is no longer blocked.
  Trigger when the user asks to update, answer, clear, or resolve the Greptile threads or comments on a PR, or when a
  ready PR with green checks still shows `mergeStateStatus: BLOCKED`.
---

# `/greptile-threads`: update a PR's Greptile threads

`main` requires every review thread resolved (`required_review_thread_resolution`), so one open Greptile thread holds
auto-merge as firmly as a red `ci`. This skill is the batch loop around [[review-pr]]. That skill settles one finding.
This one finds them all, settles each, and leaves none open. The authoritative commands are in [Address a PR
comment](../../../docs/agent-workflows.md#address-a-pr-comment).

## 1. List the open threads

```bash
gh api graphql --paginate -f query='
query($owner:String!,$repo:String!,$pr:Int!,$endCursor:String){
  repository(owner:$owner,name:$repo){ pullRequest(number:$pr){
    reviewThreads(first:100, after:$endCursor){
      pageInfo{ hasNextPage endCursor }
      nodes{ id isResolved isOutdated path line
        comments(first:20){ nodes{ databaseId author{login} body } } } } } }
}' -F owner=<owner> -F repo=<repo> -F pr=<N> \
  --jq '.data.repository.pullRequest.reviewThreads.nodes[]
        | select(.isResolved|not)
        | select(.comments.nodes[0].author.login|test("greptile"))
        | {id, path, line, isOutdated, finding: .comments.nodes[0].body, replies: (.comments.nodes|length - 1)}'
```

`--paginate` follows `endCursor` until `hasNextPage` is false, so threads beyond the first 100 are listed too. The
bodies are HTML-heavy, so strip tags before you read them. Also read Greptile's summary comment (`gh api
repos/<slug>/issues/<N>/comments`): a finding that appears only there has no thread to resolve and gets a PR comment
instead.

## 2. Settle each finding

Take the threads one at a time through [[review-pr]]: read the thread, the cited code at the PR head, and its covering
test, then classify it as valid, invalid, or valid but not worth fixing.

- **Valid**: make the minimum fix and add its covering test. Batch the fixes for one PR into a single commit when they
  touch different code, and name each finding in the message.
- **Invalid or won't fix**: change no code. The reply carries the file and line evidence.
- **Outdated** (`isOutdated: true`): the cited lines changed. Check whether the claim still holds at the head before
  you call it fixed.

Before pushing, run the focused tests for the touched crate and `cargo clippy -p <crate> --all-targets -- -D warnings`.
`navigator validate` does not run clippy.

## 3. Reply and resolve

Push first, so the reply can name a commit that is on the branch. Then reply and resolve each thread with one GraphQL
call:

```bash
gh api graphql -f query='
mutation($id:ID!,$body:String!){
  addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$id, body:$body}){ comment{ id } }
  resolveReviewThread(input:{threadId:$id}){ thread{ isResolved } }
}' -f id=<threadId> -f body='Fixed in <sha>: <what changed>. Covered by `<test name>`.'
```

A declined finding gets `Not fixing: <reason, with path:line evidence>.` instead. Resolve only the threads you settled.
A human reviewer's thread is theirs to resolve, so reply to it and leave it open.

Replies are public. Reference planning by bare issue identifier (`ENG-1234`), never by a `linear.app` URL, issue title,
or project name, and follow [public contributor safety](../../../docs/public-contributor-safety.md).

## 4. Confirm, then watch for the re-review

```bash
gh api graphql --paginate -f query='
query($owner:String!,$repo:String!,$pr:Int!,$endCursor:String){
  repository(owner:$owner,name:$repo){ pullRequest(number:$pr){
    reviewThreads(first:100, after:$endCursor){
      pageInfo{ hasNextPage endCursor }
      nodes{ id isResolved comments(first:1){ nodes{ author{login} } } } } } }
}' -F owner=<owner> -F repo=<repo> -F pr=<N> \
  --jq '.data.repository.pullRequest.reviewThreads.nodes[]
        | select(.isResolved|not)
        | select(.comments.nodes[0].author.login|test("greptile")) | .id' | wc -l
gh pr view <N> --json mergeStateStatus,autoMergeRequest
```

The command pages through every thread and counts only the unresolved threads Greptile opened. That count must be `0`. A
human reviewer's thread you left open is not counted here, but it still holds the merge under
`required_review_thread_resolution`, so name it in the report as a blocker for its reviewer. Greptile reviews again
after each push and can open new threads on the fix itself, so check again once its check run on the new head completes.
The PR is not done until that review is answered too.

## Report

Report each thread's verdict, the fix commit, the proof, and the count of open threads left. Also report anything still
blocking the merge, such as a pending `ci` or a CodeQL alert, without fixing it under this skill. For a failed check,
use [Address a failed GitHub Action](../../../docs/agent-workflows.md#address-a-failed-github-action).

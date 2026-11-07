#!/usr/bin/env bash
# Commit the working tree's changes to a new branch as a GitHub-signed commit
# and open a pull request against main.
#
#   GH_TOKEN=<app token> scripts/open-signed-pr.sh <branch> <title> <body>
#
# createCommitOnBranch makes a commit GitHub signs, which the `production`
# ruleset's required signatures accept; a runner's own `git commit` would be
# unsigned. The token must be the automerge App's, not GITHUB_TOKEN: GitHub
# starts no workflow runs for a pull request the workflow token opens.
set -euo pipefail

branch=${1:?branch required}
title=${2:?title required}
body=${3:?body required}
cd "$(git rev-parse --show-toplevel)"
base=$(git rev-parse HEAD)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

# Contents go through files, never argv: a large manifest test exceeds the
# kernel's argument-length limit as a single base64 argument.
echo '[]' > "$scratch/files.json"
while IFS= read -r path; do
  base64 < "$path" | tr -d '\n' > "$scratch/contents"
  jq --arg path "$path" --rawfile contents "$scratch/contents" '. + [{path: $path, contents: $contents}]' \
    "$scratch/files.json" > "$scratch/next.json"
  mv "$scratch/next.json" "$scratch/files.json"
done < <(git diff --name-only)

gh api "repos/$GITHUB_REPOSITORY/git/refs" -f "ref=refs/heads/$branch" -f "sha=$base" >/dev/null
jq -n --arg repo "$GITHUB_REPOSITORY" --arg branch "$branch" --arg base "$base" --arg headline "$title" \
  --arg body "$body" --slurpfile files "$scratch/files.json" '{
    query: "mutation($input: CreateCommitOnBranchInput!) { createCommitOnBranch(input: $input) { commit { oid } } }",
    variables: { input: {
      branch: { repositoryNameWithOwner: $repo, branchName: $branch },
      expectedHeadOid: $base,
      message: { headline: $headline, body: $body },
      fileChanges: { additions: $files[0] }
    } }
  }' > "$scratch/request.json"
gh api graphql --input "$scratch/request.json" >/dev/null
gh pr create --base main --head "$branch" --title "$title" --body "$body"

# Contributing

Neon Law Navigator is open source under `MIT OR Apache-2.0`, and anyone may clone, build, run, and fork
[neon-law/navigator](https://github.com/neon-law/navigator). Outside pull requests and GitHub issues are closed for now;
write to [support@neonlaw.com](mailto:support@neonlaw.com) with a bug report, contribution inquiry, or question. Report
suspected vulnerabilities through [security reporting](SECURITY.md), privately rather than in a public issue.

Accepted contributions use the project's MIT OR Apache-2.0 license. Contributors retain their copyright; no separate CLA
is required. Read [contributor terms](CONTRIBUTORS.md) for the contribution terms, recommended provenance checks, and
AI-assisted work. A merge makes no representation that a contribution was reviewed or verified.

## Local setup

Start with [self-hosting](docs/self-hosting.md#fresh-clone-to-a-running-web). It lists the prerequisites and takes a
fresh clone to a running local `web` with no private credentials, cloud account, or registry login.

## Source and review

Publish only source and firm-owned or synthetic examples. Use reserved example domains for non-firm identities. Never
put client data, legal files, real contact details, or production identifiers in Git, issues, pull requests, or public
captures. See [public contributor safety](docs/public-contributor-safety.md).

Start changes in an isolated worktree. Run focused tests while implementing, then `cargo run -p cli --quiet -- validate`
and `git diff --check`. Keep changes scoped and describe their validation in the pull request.

Legal template bodies require licensed attorney review before merge. A source contribution or use of a template does not
create an attorney-client relationship or establish that generated work is legally correct.

See [project license](LICENSE), [attribution notice](NOTICE), and [licensing](docs/licensing.md) for the project grant
and separate third-party rights. Copyright notices identify their actual holders; agents must not create ownership
claims.

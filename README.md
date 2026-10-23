# Neon Law Navigator

Neon Law Navigator is an agentic lawyering system where workflows are triggered around the clock to mise en place legal
judgment for counsel so they can act faster on completing legal projects. Our collective goal to solve as many projects
as possible furthers our goal of doing what we can to improve access to justice.

If you are new, review our workshops for [lawyers](https://www.neonlaw.com/workshops/use-the-navigator), [operators or
firm owners](https://www.neonlaw.com/workshops/deploy-the-navigator), or
[engineers](https://www.neonlaw.com/workshops/contribute-to-the-navigator). Each matter is a Git repository of
declarations that Navigator renders and gates, so an agent edits YAML and Markdown, not application code.

## Supported platforms

Navigator is built for **Macs** and **containers in the cloud**. The CLI ships for Apple silicon macOS (Homebrew or a
`.tar.gz`) and x86_64 Linux, and every service image is `linux/amd64` for GKE. Windows is not supported: there is no
Windows build, release archive, or CI job.

## Run it on your machine

```bash
brew install neon-law-source-code/navigator/navigator
navigator quickstart
```

`quickstart` takes no flags. It checks the host tools, including a running Docker runtime, and prints one `brew install`
line for anything missing. It then clones this repository into `~/.navigator/quickstart`, brings up a KIND cluster with
PostgreSQL and Forgejo, and seeds the Shook Law PC firm with three sample matters. Finally it builds the website and
opens it. Every service gets its own port, and the closing summary lists them all with the sample sign-ins. Inside a
checkout, run `cargo run -p cli -- quickstart` instead. The first run builds from source and takes a while.

You can see the omnichannel inbox, social scheduling, case management, notations, and several example websites. Copy
`.env.example` to `.env` to add your own Meta, TikTok, or other provider keys, and use your own brand. To have Navigator
skinned for your firm, [contact us](https://www.neonlaw.com/contact). We help you write token-efficient prompts.

The three sample matters are cloned from public, read-only `neon-law/sample-*` snapshots. Until this repository is
public, quickstart needs read access to `neon-law/navigator`.

You can also start an agentic session and prompt "spin up the server".

## Build from a clone

To build and run from source without any Neon Law credentials, follow [self-hosting](docs/self-hosting.md):

```bash
cargo run -p cli -- dev up --path "$PWD"
set -a; source .devx/env; set +a
cargo run -p neon
```

[CONTRIBUTING](CONTRIBUTING.md) covers the change workflow.

## License

Navigator's first-party source is licensed under [MIT OR Apache-2.0](LICENSE), at your option, including original
template material. The [MIT text](LICENSE-MIT) and the [Apache-2.0 text](LICENSE-APACHE-2) are the standard texts. Both
include warranty disclaimers and limits on liability. Third-party components and fonts retain their own terms; see
[licensing](docs/licensing.md). See [contributor terms](CONTRIBUTORS.md) for accepted contribution terms and [security
reporting](SECURITY.md) for private vulnerability reports.

The source repository remains private until Forgejo is created. Per-project repositories are planned to move off GitHub.
The planned public source repository is [neon-law/navigator](https://github.com/neon-law/navigator). What is public is
the CLI: its archives, checksums, and install script are attached to the Releases of
[`neon-law-source-code/homebrew-navigator`](https://github.com/neon-law-source-code/homebrew-navigator), which holds the
Homebrew formula and no source. Container images live in a private Artifact Registry repository.

Font licensing is separate: see the [GORP Serif notice](server/public/fonts/gorp-serif/LICENSE.txt) and the [Tinos
OFL](pdf/assets/fonts/Tinos/OFL.txt).

Copyright (C) 2026 **Neon Law IP LLC**.

## Trademark

**NEON LAW** is a registered trademark, U.S. Reg. No. 6,325,650, owned by **Neon Law IP LLC**. Use is unauthorized
unless we grant you written permission.

## No legal advice

This repository is not legal advice, and using it does not create an attorney-client relationship.

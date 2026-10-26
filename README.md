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
brew install neon-law/navigator/navigator
navigator quickstart
```

`quickstart` takes no flags. It checks the host tools, including a running Docker runtime, and prints one `brew install`
line for anything missing. It then clones this repository into `~/.navigator/quickstart`, brings up a KIND cluster with
PostgreSQL and Forgejo, and seeds the Guieb & Shook PC firm with three sample matters. Finally it builds the website and
opens it. Every service gets its own port, and the closing summary lists them all with the sample sign-ins. Inside a
checkout, run `cargo run -p cli -- quickstart` instead. The first run builds from source and takes a while.

You can see the omnichannel inbox, social scheduling, case management, notations, and several example websites. Copy
`.env.example` to `.env` to add your own Meta, TikTok, or other provider keys, and use your own brand. To have Navigator
skinned for your firm, [contact us](https://www.neonlaw.com/contact). We help you write token-efficient prompts.

The source and the three sample matters are cloned anonymously from public GitHub repositories:
[`neon-law/navigator`](https://github.com/neon-law/navigator) and the read-only `neon-law/sample-*` snapshots. No GitHub
account or token is needed.

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

The source is public at [neon-law/navigator](https://github.com/neon-law/navigator). The CLI's archives, checksums, and
install script are attached to this repository's [Releases](https://github.com/neon-law/navigator/releases). `brew
install neon-law/navigator/navigator` resolves the formula in
[`neon-law/homebrew-navigator`](https://github.com/neon-law/homebrew-navigator), which holds no binaries and no source.
The [Zed extension](https://github.com/neon-law/zed-navigator-lsp) attaches the Navigator language server. The
PostgreSQL image is built from [`images/postgres`](images/postgres/README.md) and published weekly to
`ghcr.io/neon-law/neon-law-postgres`. Navigator's service images live in a private Artifact Registry repository and are
mirrored to GHCR; see [self-hosting](docs/self-hosting.md#images).

Font licensing is separate: see the [GORP Serif notice](server/public/fonts/gorp-serif/LICENSE.txt) and the [Tinos
OFL](pdf/assets/fonts/Tinos/OFL.txt).

Copyright (C) 2026 **Neon Law IP LLC**.

## Trademark

**NEON LAW** is a registered trademark, U.S. Reg. No. 6,325,650, owned by **Neon Law IP LLC**. Use is unauthorized
unless we grant you written permission.

## No legal advice

This repository is not legal advice, and using it does not create an attorney-client relationship.

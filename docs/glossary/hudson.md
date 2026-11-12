---
title: "Hudson"
description: "Hudson is Navigator's command-line tool for managing deployments and running the local development stack."
---

Navigator's **operator CLI**: the `hudson` binary, built from the `hudson` package under `deploy/`. It holds the verbs a
[Deployment Operator](deployment-operator.md) and a contributor run against infrastructure and the local stack — `dev`,
`ops`, `site`, `mail`, `notation`, `glossary`, and the rest of `hudson --help`. It links the web client, the cloud SDKs,
and the KIND tooling, so it ships as its own release archive and in the `navigator-cli` image, never in the Homebrew
tap.

Two other binaries sit beside it: `navigator`, the lean CLI for Projects, Jujutsu repositories, and `validate`; and
`scorpio`, the [Ephemeris](ephemeris.md) CLI. From a checkout, run it with `cargo run -p hudson -- <verb>`.

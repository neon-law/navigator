---
title: "Hudson"
description: "Hudson is Navigator's command-line tool for managing deployments and running the local development stack."
---

Navigator's **operator CLI**: the `hudson` binary, built from the `hudson` package under `deploy/`. It holds the verbs a
[Deployment Operator](deployment-operator.md) and a contributor run to deploy Navigator, run the local stack, and
configure the Firm — `dev`, `ops`, letterheads, social accounts, OAuth clients, secrets, and the rest of `hudson
--help`. It links the web client, the cloud SDKs, and the KIND tooling, so it ships as its own release archive and in
the `navigator-cli` image, never in the Homebrew tap.

The rule for a new verb: if it works on a Project's matter (its documents, mail, notations, pages, or glossary terms),
it is `navigator`; if it deploys Navigator or configures the Firm, it is `hudson`. `navigator site document`, `site
mail`, `site notation`, and `glossary` follow it, and `hudson` keeps those spellings as deprecated aliases for one
release. `preview`, `notation` rendering, and `mail` still link the web renderer, Typst, or the store directly, so they
stay here until they read through the site API.

Two other binaries sit beside it: `navigator`, the lean CLI for Projects, Jujutsu repositories, `validate`, and matter
work; and `scorpio`, the [Ephemeris](ephemeris.md) CLI. From a checkout, run it with `cargo run -p hudson -- <verb>`.

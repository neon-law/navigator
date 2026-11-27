---
title: "Hudson"
description: "Hudson is Navigator's operator CLI for deployments, infrastructure and local development."
---

Navigator's **operator CLI**: the `hudson` binary, built from the `hudson` package under `deploy/`. A [Deployment
Operator](deployment-operator.md) uses `ops` and `dev` for cloud access, deployment, infrastructure, migrations,
backups, recovery, and secrets provisioning.

Authorized application work belongs in `navigator`, including People, Firm and Brand administration, Legal Plans, mail
drafts, Inbox, Downloads and Notation rendering. These operations use the existing Navigator site session and key
storage. Local PDF and Word rendering shares the existing renderer; remote reads go through the site API rather than
opening a database from the user toolchain.

Hudson retains existing application spellings during the release transition so pinned repositories continue working. The
local Matter Page preview and other remaining compatibility commands must not acquire new application behavior; new
application work belongs in Navigator. The public Project toolchain never requires cloud or database credentials.

`scorpio` is the separate [Ephemeris](ephemeris.md) CLI. From a checkout, run the operator with `cargo run -p hudson --
<verb>`.

The `navigator-ops` release image carries Hudson for PostgreSQL admission and recovery and the Forgejo runner
controller. `deploy.yml` builds it once and signs its immutable digest in both registries. The explicit SurrealDB export
CronJob uses the `navigator-surreal-archive` compatibility alias of that same digest. Ordinary operator jobs require the
`navigator-ops` digest; an older alias-only release map cannot supply it. Retire the alias only after retained Source
export and recovery consumers have an accepted replacement and their owned retirement is verified.

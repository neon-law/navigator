---
title: "Hudson"
description: "Hudson is Navigator's operator CLI for deployments, infrastructure and local development."
---

Navigator's **operator CLI**: the `hudson` binary, built from the `hudson` package under `deploy/`. A [Deployment
Operator](deployment-operator.md) uses `ops` and `dev` for cloud access, deployment, infrastructure, migrations,
backups, recovery, secrets provisioning and the remaining Restate operations.

Authorized application work belongs in `navigator`, including People, Firm and Brand administration, Legal Plans, mail
drafts, Inbox, Downloads and Notation rendering. These operations use the existing Navigator site session and key
storage. Local PDF and Word rendering shares the existing renderer; remote reads go through the site API rather than
opening a database from the user toolchain.

Hudson retains existing application spellings during the release transition so pinned repositories continue working. The
local Matter Page preview and other remaining compatibility commands must not acquire new application behavior; new
application work belongs in Navigator. The public Project toolchain never requires cloud or database credentials.

`scorpio` is the separate [Ephemeris](ephemeris.md) CLI. From a checkout, run the operator with `cargo run -p hudson --
<verb>`.

---
title: "`devx`"
description: >-
  Hudson commands for the local KIND development environment.
---

The `hudson dev` commands start PostgreSQL, SeaweedFS, and Rauthy in KIND, open port-forwards, and write `.devx/env`.
The host-side application reads that environment to serve the portal and DBOS.

```bash
cargo run --release -p hudson -- dev up      # bring it all up
set -a; source .devx/env; set +a       # connection env vars
cargo run -p neon                       # host-side web on :3001
cargo run --release -p hudson -- dev down    # tear it all down
```

Subcommands:

- `dev up` starts the KIND dependencies and writes the environment for the host application.
- `dev down` — kill port-forwards and delete the KIND cluster. `dev env`, `dev status` — print the env file / show
  whether port-forwards are alive. `dev kind up` and `dev kind down` manage the KIND cluster and ingress. `dev deploy`
  seeds an isolated PostgreSQL database and runs the published `neon-server` image behind the KIND ingress. Set
  `NAVIGATOR_IMAGE_TAG` to pin a release.
- `dev undeploy` removes the disposable demo and its data. `dev worktree-env up/down/status` manages the worktree's
  dependency tier and host ports (`--branch` branches a supplied checkout in place or creates a sibling worktree;
  `--demo` runs the in-cluster stack). `dev e2e` smoke-tests rollouts, `/health`, embedded Rego decisions, and local
  seed counts. `dev grant-lawyer` pre-seeds the Lawyer demo user for browser e2e with the `lawyer` role. `ops ship` —
  one-shot production roll that pins service deployments to a named `--tag`. `dev logs` tails application logs.

Use `hudson dev --help` for the available local commands.

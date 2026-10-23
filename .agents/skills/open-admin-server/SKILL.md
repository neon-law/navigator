---
name: open-admin-server
description: Prepare a local authenticated administrator session for Navigator development.
---

# Open local admin

Read [`kind-local-dev`](../kind-local-dev/SKILL.md), [`docs/access-model.md`](../../../docs/access-model.md), and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).

## Grant Lawyer and sign in

Authentication comes from OIDC; authorization comes from `persons.role`. A first verified sign-in with a supported
provider and an email creates a Client without Project participation. The configured `NAVIGATOR_BOOTSTRAP_OWNER_EMAIL`
creates an Owner. A local development Lawyer still needs a role grant before signing in. Seed Lawyer into the worktree's
environment-owned `navigator` database:

```bash
cargo run -p cli -- dev grant-lawyer
```

Open `NAV_BASE_URL` from `.devx/env`, follow `/auth/login`, and sign in through Rauthy. A firm tier lands on
`/app/team`; a client lands on `/app/projects`. `/app/*` requires authentication, and `/app/lawyer/*` also requires the
database role `lawyer` or `admin`. The session carries the role, so sign in again after any role change.

[`docs/access-model.md`](../../../docs/access-model.md) lists the local Rauthy fixture's five role-named accounts. Its
admin surface is `http://localhost:30080/auth/v1/admin` on the shared tier, or the Rauthy port printed for a worktree.
These credentials exist only in the loopback-only KIND fixture; the staging layer contains none.

`GET /auth/logout` clears the app session and, when the provider publishes an `end_session_endpoint`, redirects through
it so Rauthy drops its SSO session too. The next `/auth/login` then prompts for credentials.

- Use only the documented local KIND environment and fixture identities; never hand-write a session cookie.
- Confirm the active context is local or staging before inspecting it. Do not access or change production.
- Treat all screenshots, logs, and examples as public: no client data, legal files, real contact details, or
  production identifiers.

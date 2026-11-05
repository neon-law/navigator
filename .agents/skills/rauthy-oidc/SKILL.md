---
name: rauthy-oidc
description: >-
  Maintain local and staging OIDC discovery, fixtures, callbacks, token validation, and identity linking. Keep providers
  configurable.
---

# Rauthy and OIDC

Rauthy is the local and staging IdP; production uses Google Identity Services. OIDC discovery is the contract between
them, so provider changes belong in environment configuration and manifests, not application branches.

[`docs/oidc.md`](../../../docs/oidc.md) is authoritative for the login sequence, local fixture, single-public-issuer
bridge, role/session boundary, tests, and troubleshooting. Read it before acting.

## Rules

- **Identity is `sub` + `email`.** The `persons` table owns profile, role, project membership, and billing.
- **The session role comes from the database.** At `/auth/callback`, link the identity to a `persons` row and stamp
  `session.role = row.role`; ignore token-side authorization claims.
- **Use discovery.** Never hardcode authorization, token, or JWKS endpoints in application code.
- **Keep one issuer locally.** Rauthy's `PUB_URL`, `.devx/env`, and the in-cluster loopback bridge must all derive from
  the selected Rauthy port.
- **Verify the id_token fully.** Validate the JWKS signature plus `iss`, `aud`, `exp`, and `nbf`. Rauthy and Google use
  RS256; only mocked tests accept HS256.
- **Keep credentials environment-owned.** `k8s/staging/rauthy.yaml` contains no usable bootstrap or client secret. The
  known lawyer, Virgo, client, and administrator credentials live only in the loopback-bound KIND fixture.

## Anti-patterns

- Reading application profile or authorization from id_token claims.
- Using Rauthy roles for Navigator authorization.
- Giving browser and backchannel requests different issuer URLs.
- Adding provider-specific routes or rewrites to `portal/src/oauth.rs`.
- Storing the access token after extracting the login identity.

## Boundaries

- Role and participation model: [[authorization-model]] and `docs/access-model.md`.
- Rego decision point: [[opa-policy]].
- Local KIND lifecycle: [[kind-local-dev]].

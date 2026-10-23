---
name: opa-policy
description: >
  Embedded Rego (OPA policy language, evaluated in-process by Regorus) as the authorization decision point for `web`.
  Trigger when writing or modifying Rego (`portal/policy/navigator.rego` or its `_test.rego`), touching
  `portal::policy::require_policy` or `PolicyClient`, adding a new `/app/...` route that needs policy enforcement, or
  when someone mentions OPA. Also trigger before reaching for a role-based authorization library: authz routes through
  Rego.
---

# Embedded Rego authorization

The compiled policy is the **decision point**; `require_policy` in `web` is the **enforcement point**. Two docs are
authoritative; read them before acting:

- [`docs/rego-policy.md`](../../../docs/rego-policy.md): the checked-in policy and tests, boot-time compilation,
  authoring, the `regorus_policy` parity gate, and the Rust boundary (`PolicyClient`, `require_policy`).
- [`docs/access-model.md`](../../../docs/access-model.md#how-embedded-rego-decides): the canonical `input` document,
  the allow rules, admin bypass, and project scoping. This is the source of truth for what a rule *decides*.

## Rules

- **`default allow := false`**, with one `route_allow` rule per intent. Final `allow` also enforces CI
  matter-content denial across every tier.
- **Fail closed.** Only a boolean `true` admits; undefined, non-boolean, and evaluation-error results deny. A malformed
  policy blocks readiness. `PolicyClient::passthrough` is for test fixtures only.
- **Authz lives in Rego, not Rust.** No `if path.starts_with(...) && session.role != "lawyer"` in handlers. Rego decides
  "is this allowed"; the store stays the source of truth for the protected data.
- **One middleware, not per-handler calls.** Apply `require_policy` once at router boundaries with route-shape input.
- **Policy ships with its routes.** There is no sidecar, policy endpoint, or policy environment variable. Prove a change
  with `cargo test -p cli --test regorus_policy`.
- **Single `role`, not a `roles` array.** The session carries one `role` (see [[authorization-model]]). Rewrite any
  `roles[…]` shape in `input` or Rego to the singular field.

## Boundaries

- The role + participation model and "who can see what": [[authorization-model]] and `docs/access-model.md`.
- How the session and its `role` get populated at login: [[rauthy-oidc]] and `docs/oidc.md`.

---
title: "House Brand"
description: "A House Brand is a compiled routing key that maps a request's host to its public site and presentation."
---

A compiled routing key: one variant of [`views::brand::BrandKey`](../../views/src/brand.rs). **The source of truth is
the code**, not a table: `BrandKey::ALL` lists every House Brand, and `store::firms::CLOSED_BRAND_KEYS` is its string
form for the `store` crate, which does not depend on `views`. Adding one is a code change to the registry (a new key,
its hosts, its `Branding`) with a covering test, which is the right cost for a public legal identity, and there is no
runtime flag that can move a page from one House Brand's hosts to another's.

Each House Brand resolves its own [`Branding`](../../views/src/brand.rs): hosts, fonts, logos, and copy. One
`neon-server` process serves every compiled key; its [`registered_brand_key`](../../views/src/brand.rs) resolver maps
each request's `Host:` header to a key. **A House Brand is a registry entry, not a binary.**

Every House Brand also has a [Brand](brand.md) row with the same [Brand Key](brand-key.md), seeded on first boot by
`store::seed`, so a Firm can wear it and a Project can name it. The reverse does not hold: a Brand a Firm's Admin DRI
creates at runtime has no House Brand, so it routes no host and publishes no marketing page.

`portal::canonical_host::resolve_brand_and_enforce_host` resolves the key early in the middleware stack from the
incoming `Host:` header and stashes it as a request extension; `scope_branding` reads that extension and scopes the
resolved `Branding` for the rest of the request, the same [`views::brand::scope`](../../views/src/brand.rs) task-local
mechanism a mounted white-label bundle already used to scope its own `Branding`. An unregistered host redirects to the
deployment's own configured host (`CANONICAL_HOST`); the `/app/health` and `/app/readyz` probes answer on every host,
unredirected.

Public footer social links come from the request-scoped `Branding.firm_social` list. Each House Brand carries the same
firm YouTube channel; other destinations appear only on the House Brand whose public profile was verified. Vesta carries
its Facebook, Instagram, Threads, and TikTok profiles. A renamed firm inherits none. These public links are presentation
metadata, separate from an authorized [Social Account](social-account.md): displaying a profile does not enable API
publishing.

Distinct from [`portal::hosting::Site`](../../portal/src/hosting.rs) (named apart from the per-request identity above):
a `Site` is what one brand *crate*'s `main` hands the shared run loop — its telemetry service name and the public routes
and Dioxus routers it composes. Two shapes ship:

- **`neon`** — [the whole site](../../neon/src/lib.rs), the `neon-server` image. It composes the public routes for every
  House Brand the registry names and mounts the identical Navigator application beneath them, and it is the only binary
  that mounts the [Presentation](presentation.md) and workshop catalogs.
- **`tenant`** — [the white-label shape](../../portal/src/tenant.rs), which publishes no public face at all and
  redirects its bare host into the portal. It lives inside `portal` rather than in a crate of its own because a tenant
  has no public site to compose — that is the entire point.

Distinct again from [`views::brand::SiteBrand`](../../views/src/brand.rs) (`FIRM_BRAND`), the presentation half: the
strings, nav links, and footer attribution a page renders for whichever `Branding` is scoped to the request, overridable
by a mounted `BrandManifest` for the `neon` key alone. `BrandKey` names *which identity a request resolved to*, `Site`
names *the serving binary's own composition*, and `SiteBrand` names *what the page says*. A [Firm](firm.md) is none of
those three: it is the owning practice beneath the brand registry.

- Deployment map: [`environments`](../environments.md#why-the-brand-is-the-image)
- Source: [`views::brand`](../../views/src/brand.rs)

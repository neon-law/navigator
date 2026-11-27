---
title: "Sample Matter Fixture"
description: "A Sample Matter Fixture is one of the three synthetic matters applied alongside the canonical seed."
---

The three synthetic matters a boot applies on top of the canonical seed wherever `NAVIGATOR_SIMULATED_MATTERS` resolves
true. Written by [`store::seed::seed_sample_portfolio`](../../store/src/seed.rs), idempotent, and it keeps the local
accounts and all three matters ready for the firm, clerk, and client surfaces.

The three are deliberately different shapes of legal work, because one matter can only demonstrate one:

| Code | Matter | Practice | Client |
| --- | --- | --- | --- |
| `sample-litigation` | *Cruller v. Prine* | trespass and rescission | an individual plaintiff |
| `sample-transactional` | *Widget Works — Outside Counsel* | employment and contract review | a Nevada C-Corp |
| `sample-estate` | *Estate of Bigfoot* | a probate | an heir of the decedent |

Each Project names its own repository, and the seed reads its `pages:` and `drafts/` from it. Local development clones a
public, read-only snapshot at `neon-law/<code>` on GitHub, so `hudson quickstart` clones it without credentials; the
snapshots run no Actions. Staging's source of truth is the Project's repository on that deployment's own Forgejo
(`<firm-slug>/<code>`), where changes land by pull request.

The project code is the URL slug: lowercase letters and numbers joined by single hyphens, with no UUID in the project
show URL.

A `dev` boot with no built bundle staged publishes a deterministic placeholder document, which is what keeps a portal
serving something while a Vite build is broken. A boot under the **production** deployment profile publishes nothing:
whatever sits in that deployment's applications bucket was published by an operator and is authoritative, so the seed
leaves it alone and an unpublished portal answers 404 rather than a placeholder that looks like a working application.
That is the one place [Deployment Environment](deployment-environment.md) reaches past which rows get seeded and into
what gets written to object storage.

The fixture Client participates in all three matters and sees each one on its Brand's portal. The fixture Admin has no
Project participation. [Deployment Environment](deployment-environment.md) lists the deployments that use this seed.

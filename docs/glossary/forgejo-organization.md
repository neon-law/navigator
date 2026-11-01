---
title: Forgejo Organization
description: >-
  A Firm's organization on the installation's Forgejo, named by the Firm's slug and provisioned by Navigator.
---

A **Forgejo Organization** is the namespace a [Firm](firm.md)'s Project [repositories](repository.md) live in on the
installation's Forgejo, at `<firm-slug>/<project-code>`. It is named by the Firm's immutable `slug`, so renaming the
Firm never moves it. Each Firm has at most one.

The installation lets only a site administrator create organizations, so Navigator's provisioning identity creates or
reconciles it (`navigator site firm provision-forgejo <firm-slug>`). That identity also keeps the account the Firm's own
Forgejo access token authenticates in the organization's owners team. Re-running converges: nothing is created or added
twice. See [Firm organizations](../forgejo-deployment.md#firm-organizations).

Navigator keeps one PostgreSQL row per Firm in `firm_forgejo_organization`, recording the organization name, Forgejo's numeric
`forgejo_id`, the `owner_login` kept in the owners team, the first `provisioned_at`, and the latest `reconciled_at`.

- Vocabulary: [`store::firms::forgejo_organization`](../../store/src/firms/forgejo_organization.rs)

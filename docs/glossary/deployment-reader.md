---
title: Deployment Reader
description: "A Deployment Reader is a read-only service account declared in a deployment's access file."
---

## Deployment Reader

A service account that may observe a deployment's project without changing it: an outside agent diagnosing staging, or a
dashboard. It is declared as a `[[reader]]` in the deployment's `access.toml` and converged by `hudson ops gcp access`.

A reader holds only allowlisted read-only project roles and `secretAccessor` on the secrets it names, granted per secret
and never project-wide. It cannot read Data Access audit logs, so every secret it reads is recorded where it cannot see.
Revoke a reader by disabling its account or deleting its key.

See [Access, encryption, and audit](../deployment-secrets.md#access-encryption-and-audit).

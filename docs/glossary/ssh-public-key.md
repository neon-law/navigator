---
title: "SSH Public Key"
description: >-
  The public half of a Firm user's SSH key pair, registered once in Navigator and reconciled onto their Forgejo account.
---

A Firm user's SSH public key, owned by one [Person](person.md) and stored in the PostgreSQL `ssh_public_key` table. A
Person registers a key once, on their profile page (`/app/profile`) or with `navigator site person ssh-key add`, and
Navigator makes it a key on their account on the installation's [Forge](forge.md). Only the public half is accepted:
`store::ssh_public_keys::parse` refuses anything that looks like a private key, admits only `ssh-ed25519` and the three
NIST ECDSA curves (`ecdsa-sha2-nistp256`, `-nistp384`, `-nistp521`), checks that the base64 body is a well-formed key of
its declared type, and stores the canonical `<algorithm> <base64>` line without its comment.

Each row carries the OpenSSH `SHA256:` `fingerprint`, a `label` (the key's comment unless the Person names one),
`created_at`, and `last_synced_at`, the last time a reconciliation of the Person's Forgejo account completed. A
fingerprint is live on at most one row in the installation, so the same key is refused for a second Person.

Navigator's list is authoritative only while the Person holds a [Person–Firm Role](personfirm-role.md). The
reconciliation (`store::ssh_public_keys::sync_person`) adds every live key missing from the Forgejo account and removes
every key Navigator put there that it no longer holds; with no Firm membership the desired list is empty, so leaving a
Firm takes the keys off the forge. Keys Navigator added carry a `navigator:` title prefix, and only those are ever
removed. A key on the account that Navigator neither holds nor manages is drift: it is reported by fingerprint and never
silently removed.

Removing a key is a revocation first. `revoked_at` (and `revoked_by_person_id`) takes it out of the desired list at
once, and the row is deleted only after a reconciliation has removed the key from Forgejo, so a sync that fails part-way
still leaves the operator sweep (`navigator ops forgejo ssh-keys sync`) a row to revisit. A Person manages only their
own keys; an Owner, or an Admin who may manage one of the key owner's Firm memberships, may also revoke one.

The reconciliation runs under a dedicated key-sync service identity, never the reserved `navigator-admin` seat or an
operator's own account; see [Forgejo beside Navigator](../forgejo-deployment.md#ssh-public-keys).

- Vocabulary: [`store::ssh_public_keys`](../../store/src/ssh_public_keys.rs) ·
  [`cloud::forgejo_user_keys`](../../cloud/src/forgejo_user_keys.rs)

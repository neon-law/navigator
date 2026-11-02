-- A Firm user's SSH public keys, the authoritative list Navigator reconciles
-- onto the Person's account on the installation's Forgejo (ENG-1219). Only
-- the public half is ever stored: the canonical `<algorithm> <base64>` line,
-- with any comment dropped. See docs/glossary/ssh-public-key.md.
--
-- Removal is a revocation first: `revoked_at` takes the key out of the
-- desired set at once, and the row is deleted only after a reconciliation
-- has removed the key from Forgejo, so a sync that fails part-way still
-- knows which Person's account to revisit.
CREATE TABLE ssh_public_key (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    person_id uuid NOT NULL REFERENCES person (id) ON DELETE CASCADE,
    algorithm text NOT NULL,
    public_key text NOT NULL,
    fingerprint text NOT NULL,
    label text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_synced_at timestamptz,
    revoked_at timestamptz,
    revoked_by_person_id uuid REFERENCES person (id) ON DELETE SET NULL,
    CONSTRAINT ssh_public_key_algorithm CHECK (algorithm IN ('ssh-ed25519', 'ecdsa-sha2-nistp256', 'ecdsa-sha2-nistp384', 'ecdsa-sha2-nistp521')),
    CONSTRAINT ssh_public_key_line CHECK (public_key LIKE algorithm || ' %' AND char_length(public_key) <= 1024),
    CONSTRAINT ssh_public_key_fingerprint CHECK (fingerprint LIKE 'SHA256:%'),
    CONSTRAINT ssh_public_key_label CHECK (char_length(label) BETWEEN 1 AND 100),
    CONSTRAINT ssh_public_key_revocation CHECK (revoked_by_person_id IS NULL OR revoked_at IS NOT NULL)
);

-- One live registration per key anywhere in the installation: the same
-- fingerprint is refused for a second Person (or twice for one Person). A
-- revoked row awaiting its Forgejo removal does not block re-registration.
CREATE UNIQUE INDEX ssh_public_key_live_fingerprint
    ON ssh_public_key (fingerprint) WHERE revoked_at IS NULL;

-- The sync and the profile page read one Person's keys.
CREATE INDEX ssh_public_key_person_idx ON ssh_public_key (person_id);

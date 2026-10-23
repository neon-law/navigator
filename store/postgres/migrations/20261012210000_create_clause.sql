-- The shared Clause library: legal prose a Template includes by `code@version`.
-- `rules/clauses/library.yaml` is the source of truth; this table is its read
-- model. Versioned by append: a change adds a row, and `is_current` marks the
-- live version of each code. A shared catalog, owned by no Firm or Project.
CREATE TABLE clause (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT NOT NULL CHECK (code ~ '^[a-z0-9_]+$'),
    version INTEGER NOT NULL CHECK (version > 0),
    kind TEXT NOT NULL,
    inputs JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(inputs) = 'object'),
    body TEXT NOT NULL,
    body_sha256 TEXT NOT NULL CHECK (body_sha256 = encode(sha256(convert_to(body, 'UTF8')), 'hex')),
    is_current BOOLEAN NOT NULL DEFAULT false,
    retired_at TIMESTAMPTZ,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (code, version),
    CHECK (NOT (is_current AND retired_at IS NOT NULL))
);

CREATE UNIQUE INDEX clause_one_current_per_code ON clause (code) WHERE is_current;

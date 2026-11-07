-- Migration: mail_signature — the signature an attorney's mail drafts wear.
-- Born in PostgreSQL with no Surreal source. One row per Person, keyed by the
-- Person's id rather than their email, so a changed address never loses the
-- block that signs their mail. No foreign key on purpose: the Person row may
-- still be authoritative in SurrealDB, so the writer authorizes the actor as
-- the subject before it writes. The body is the signature text exactly as it
-- should appear above the `-- ` delimiter the draft command manages, so a
-- stray delimiter line of its own is refused here.
CREATE TABLE IF NOT EXISTS mail_signature (
    person_id UUID PRIMARY KEY,
    body TEXT NOT NULL
        CHECK (body = trim(body) AND body <> '' AND length(body) <= 2000 AND body !~ '(^|\n)--[[:space:]]*(\n|$)'),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

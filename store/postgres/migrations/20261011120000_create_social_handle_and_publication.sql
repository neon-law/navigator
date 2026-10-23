-- PostgreSQL's Social Handle and Social Publication, the two tables a Post
-- needs beside itself. A Social Handle is the public name a Brand holds on one
-- provider; a Post's social link may only name a handle recorded here. A Social
-- Publication is one Post's delivery state on one provider. The Surreal source
-- keys a publication by a stable string; here the row's identity is the unique
-- (post_id, provider) pair, which is what makes a repeated send idempotent.
CREATE TABLE social_handle (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    brand_key TEXT NOT NULL REFERENCES brand(brand_key),
    provider TEXT NOT NULL CHECK (provider IN ('facebook', 'instagram', 'threads', 'youtube', 'linkedin', 'x', 'tiktok')),
    handle TEXT NOT NULL CHECK (handle ~ '^[A-Za-z0-9._-]{1,64}$'),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (brand_key, provider)
);

CREATE TABLE social_publication (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    post_id UUID NOT NULL REFERENCES post(id),
    provider TEXT NOT NULL CHECK (provider IN ('facebook', 'instagram', 'threads', 'youtube', 'linkedin', 'x', 'tiktok')),
    state TEXT NOT NULL CHECK (state IN ('sending', 'unknown', 'failed', 'confirmed')),
    attempt_count INTEGER NOT NULL CHECK (attempt_count > 0),
    provider_task_id TEXT,
    provider_post_id TEXT,
    permalink TEXT,
    last_error TEXT,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (post_id, provider)
);

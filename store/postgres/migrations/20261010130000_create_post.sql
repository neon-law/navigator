-- PostgreSQL's Brand-owned Post, named from the ontology and glossary.
-- The Surreal source table was also renamed from `blog_post` to `post`; this
-- target migration keeps the same public contract while PostgreSQL parity is
-- still being built table by table.
CREATE TABLE post (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    brand_key TEXT NOT NULL REFERENCES brand(brand_key),
    slug TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    body_html TEXT NOT NULL DEFAULT '',
    body_markdown TEXT,
    status TEXT NOT NULL CHECK (status IN ('draft', 'scheduled', 'published', 'cancelled')),
    publish_at TIMESTAMPTZ,
    video_asset TEXT,
    tiktok_ai_generated BOOLEAN,
    vertical_video BOOLEAN NOT NULL DEFAULT false,
    video_orientation TEXT CHECK (video_orientation IS NULL OR video_orientation IN ('vertical', 'landscape', 'square')),
    source_digest TEXT,
    social_links JSONB NOT NULL DEFAULT '[]'::jsonb,
    facebook_caption TEXT,
    instagram_caption TEXT,
    threads_caption TEXT,
    youtube_caption TEXT,
    selected_social_accounts TEXT[] NOT NULL DEFAULT '{}',
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (brand_key, slug)
);

CREATE INDEX post_brand_status_publish_at ON post (brand_key, status, publish_at);

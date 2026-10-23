CREATE TABLE blog_video (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    brand_key TEXT NOT NULL REFERENCES brand(brand_key),
    object_key TEXT NOT NULL UNIQUE,
    width BIGINT NOT NULL CHECK (width > 0),
    height BIGINT NOT NULL CHECK (height > 0),
    orientation TEXT NOT NULL CHECK (orientation IN ('vertical', 'landscape', 'square')),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

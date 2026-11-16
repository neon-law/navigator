CREATE TABLE inbox_attachment_filing (
    id UUID PRIMARY KEY,
    message_id UUID NOT NULL REFERENCES message(id),
    project_id UUID NOT NULL REFERENCES project(id),
    attachment_asset_id UUID NOT NULL REFERENCES asset(id),
    approval JSONB NOT NULL UNIQUE,
    attempt INTEGER NOT NULL DEFAULT 1 CHECK (attempt > 0),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'filed', 'failed')),
    result_asset_id UUID REFERENCES asset(id),
    failure_code TEXT,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((state = 'filed') = (result_asset_id IS NOT NULL)),
    CHECK ((state = 'failed') = (failure_code IS NOT NULL))
);
CREATE INDEX inbox_attachment_filing_pending
    ON inbox_attachment_filing (inserted_at, id) WHERE state = 'pending';

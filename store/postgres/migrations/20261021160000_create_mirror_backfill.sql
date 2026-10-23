-- Operator runs that copy SurrealDB rows the PostgreSQL write mirror never
-- saw, in bounded batches. Born in PostgreSQL. A run resumes by its id; each
-- mirror group's checkpoint records the batches written, and a failure names
-- its row by table and UUID only, with a sanitized reason.
CREATE TABLE mirror_backfill_run (
    id UUID PRIMARY KEY,
    batch_size INTEGER NOT NULL CHECK (batch_size > 0),
    status TEXT NOT NULL CHECK (status IN ('running', 'completed')),
    completed_at TIMESTAMPTZ,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((status = 'completed') = (completed_at IS NOT NULL))
);

CREATE TABLE mirror_backfill_checkpoint (
    run_id UUID NOT NULL REFERENCES mirror_backfill_run(id) ON DELETE CASCADE,
    mirror TEXT NOT NULL CHECK (mirror <> ''),
    batches INTEGER NOT NULL DEFAULT 0 CHECK (batches >= 0),
    applied INTEGER NOT NULL DEFAULT 0 CHECK (applied >= 0),
    failed INTEGER NOT NULL DEFAULT 0 CHECK (failed >= 0),
    completed_at TIMESTAMPTZ,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, mirror)
);

CREATE TABLE mirror_backfill_failure (
    id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES mirror_backfill_run(id) ON DELETE CASCADE,
    mirror TEXT NOT NULL CHECK (mirror <> ''),
    row_table TEXT NOT NULL CHECK (row_table <> ''),
    row_id TEXT,
    reason TEXT NOT NULL CHECK (reason <> ''),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX mirror_backfill_failure_run ON mirror_backfill_failure (run_id, mirror);

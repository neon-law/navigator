-- Full-row PostgreSQL destination for the `person_phone` source table.

-- Direct record links retain UUIDs. This link column has no PostgreSQL FK so
-- source orphans remain importable and are reported separately.

CREATE TABLE "person_phone" (
    "id" UUID NOT NULL PRIMARY KEY,
    "e164" TEXT NOT NULL,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "is_primary" BOOLEAN NOT NULL,
    "label" TEXT NOT NULL,
    "person_id" UUID NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

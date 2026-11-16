-- An Attorney Calendar: a Google Calendar id recorded on a Person so the
-- Firm can read free/busy, and so that Person can read their own events.
-- `primary` is the one calendar whose event details the signed-in Person may
-- read. `freebusy` is an extra calendar they chose to share as busy time.
-- A Google OIDC subject is not a calendar id (docs/glossary/attorney-calendar.md).
CREATE TABLE person_calendar (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    person_id UUID NOT NULL REFERENCES person(id) ON DELETE CASCADE,
    calendar_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('primary', 'freebusy')),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (person_id, calendar_id)
);
CREATE UNIQUE INDEX person_calendar_one_primary
    ON person_calendar (person_id)
    WHERE kind = 'primary';

-- A durable running Timer on a matter: the attorney's live stopwatch, whose
-- durable row is the source of truth (a page script only animates the
-- display). The row is inserted when the Timer starts and deleted the moment
-- stop_timer captures its elapsed minutes as a Time Entry or discard_timer
-- cancels it — a Timer never rests in a stopped state, and its durable
-- billing artifact is the Time Entry it created. See docs/glossary/timer.md.
CREATE TABLE timer (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    person_id uuid NOT NULL REFERENCES person (id) ON DELETE CASCADE,
    project_id uuid NOT NULL REFERENCES project (id) ON DELETE CASCADE,
    started_at timestamptz NOT NULL DEFAULT now(),
    note text,
    CONSTRAINT timer_note_length CHECK (note IS NULL OR char_length(note) <= 500)
);

-- The firm-wide single-open-Timer invariant: at most one open Timer per
-- attorney, on any matter. A Timer row exists only while it runs, so
-- uniqueness on the attorney IS the invariant. start_timer pre-checks the
-- same condition for a friendly error; the index is the backstop against a
-- race between two tabs.
CREATE UNIQUE INDEX timer_one_open_per_attorney ON timer (person_id);

-- The matter header reads open Timers by attorney (covered above); the
-- Billing tab reads them by matter. Timers carry no audit-event table of
-- their own: the row is transient by design, and the durable billing
-- artifact is the Time Entry stop_timer captures — audited there.
CREATE INDEX timer_project_idx ON timer (project_id);

-- Durable state for the Notation state machines on DBOS.
--
-- The Restate virtual object held four keyed values per notation — the
-- questionnaire and workflow spec YAML plus each machine's current state.
-- DBOS has no virtual-object primitive, so the port moves those keys into
-- this table: one row per `(notation_id, machine_kind)`, read and written
-- by the DBOS transition workflow while it holds the notation's advisory
-- lane (see `store::notation_machine_state::acquire_lane`).
--
-- Rows are imported from the Restate Cloud export during the state drain
-- (`store::notation_machine_state::import`), which never overwrites an
-- existing row: once a machine lives here, Postgres — not the export — is
-- its source of truth.
CREATE TABLE notation_machine_state (
    notation_id UUID NOT NULL,
    machine_kind TEXT NOT NULL
        CHECK (machine_kind IN ('questionnaire', 'workflow')),
    spec_yaml TEXT NOT NULL,
    current_state TEXT NOT NULL,
    ephemeral BOOLEAN NOT NULL DEFAULT false,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (notation_id, machine_kind)
);

-- The NEF pipeline's recorded outcome beside the route: filed, a text-only
-- filing, a rerun over an already-recorded entry, or one of the triage
-- results that leaves the message in the backlog. NULL for messages that
-- were never processed through the NEF pipeline.
ALTER TABLE message ADD COLUMN nef_outcome TEXT CHECK (nef_outcome IS NULL OR nef_outcome IN ('filed', 'text_only', 'already_recorded', 'no_project', 'not_a_notice', 'no_case_number'));

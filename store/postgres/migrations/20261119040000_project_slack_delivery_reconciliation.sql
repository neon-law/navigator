-- Operator attribution and private evidence are distinct from captured Source evidence.
CREATE TABLE project_slack_delivery_reconciliation (
    operation_id UUID PRIMARY KEY,
    -- Historical attribution survives canonical deletion of an empty Project.
    project_id UUID NOT NULL,
    ownership_epoch UUID NOT NULL REFERENCES durable_object_ownership(ownership_epoch),
    actor_person_id UUID NOT NULL REFERENCES person(id),
    evidence_reference UUID NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('posted', 'verified_not_posted')),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, ownership_epoch),
    CHECK (operation_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CHECK (evidence_reference <> '00000000-0000-0000-0000-000000000000'::uuid)
);

CREATE FUNCTION preserve_project_slack_delivery_reconciliation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Project Slack delivery reconciliation is immutable';
END;
$$;

CREATE TRIGGER project_slack_delivery_reconciliation_immutable
BEFORE UPDATE OR DELETE ON project_slack_delivery_reconciliation
FOR EACH ROW EXECUTE FUNCTION preserve_project_slack_delivery_reconciliation();

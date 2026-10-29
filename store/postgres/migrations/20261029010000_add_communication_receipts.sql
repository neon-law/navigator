ALTER TABLE communication ADD COLUMN counterparty_role TEXT CHECK (counterparty_role IN ('opposing_counsel', 'client', 'court', 'other'));
ALTER TABLE communication ADD COLUMN acknowledges UUID REFERENCES communication(id) ON DELETE SET NULL;
CREATE INDEX communication_acknowledges ON communication (project_id, acknowledges);

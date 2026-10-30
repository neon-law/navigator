-- A Firm Library version: the Firm whose library repository the Template was synced from.
ALTER TABLE template ADD COLUMN firm_id UUID REFERENCES firm(id);
ALTER TABLE template ADD CONSTRAINT template_one_scope CHECK (project_id IS NULL OR firm_id IS NULL);

ALTER TABLE project_deadlines ADD COLUMN due_on JSONB;
ALTER TABLE project_deadlines ADD COLUMN hearing JSONB;
ALTER TABLE project_deadlines DROP CONSTRAINT project_deadlines_category_check;
ALTER TABLE project_deadlines ADD CONSTRAINT project_deadlines_category_check CHECK (category IN ('court', 'hearing', 'discovery', 'internal', 'tickler'));

-- Deadline categories: what kind of date the deadline is, rendered by the
-- matter calendar's fixed category-to-color map. Nullable: rows written
-- before categories existed read back without one.
ALTER TABLE project_deadlines ADD COLUMN category TEXT CHECK (category IN ('court', 'hearing', 'discovery', 'internal'));

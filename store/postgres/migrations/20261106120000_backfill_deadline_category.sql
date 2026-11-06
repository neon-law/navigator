-- Recover only categories determined by the stored facts. A hearing role
-- wins over internal authority; court and discovery require lawyer review.
DO $$
BEGIN
UPDATE project_deadlines
SET category = CASE WHEN calendar_role = 'hearing' THEN 'hearing' ELSE 'internal' END
WHERE category IS NULL AND (calendar_role = 'hearing' OR authority_kind = 'internal');
END $$;

-- ARAG also refers contested divorce (with or without custody), custody, and
-- probate matters, so the closed Plan Matter Type vocabulary widens in both
-- tables that CHECK it.
ALTER TABLE plan_fee_schedule DROP CONSTRAINT plan_fee_schedule_matter_type_check;
ALTER TABLE plan_fee_schedule ADD CONSTRAINT plan_fee_schedule_matter_type_check CHECK (matter_type IN ('document_review', 'letter', 'uncontested_divorce', 'court_appearance', 'contested_divorce', 'custody', 'probate'));
ALTER TABLE project DROP CONSTRAINT project_plan_matter_type_check;
ALTER TABLE project ADD CONSTRAINT project_plan_matter_type_check CHECK (plan_matter_type IN ('document_review', 'letter', 'uncontested_divorce', 'court_appearance', 'contested_divorce', 'custody', 'probate'));

-- The legal-plan panel economics a plan matter carries (ENG-1269). Every
-- column is NULL until a plan assignment stages it, which is also how
-- reporting separates the Firm's three matter lines: commercial litigation
-- and pro bono leave all nine unset.
--
-- `legal_plan_id` points into the Firm's own `legal_plan` vocabulary. The
-- accessor ([`store::projects`]`::set_plan_columns`) proves the plan belongs
-- to the Project's Firm before writing; the foreign key is the backstop for
-- a writer that skips the accessor, the same role `contingency_term`'s
-- trigger plays for its tiers. The other plan columns are plain values by
-- design: a legacy or mirrored row always reads back.
--
-- `plan_matter_type` shares the closed vocabulary `plan_fee_schedule`
-- enforces and the assumption keys name, CHECKed here for any writer.
-- `plan_state` is a lowercase jurisdiction code as `plan_fee_schedule.state`
-- holds it. The three plan dates are DATE, and `kit_version` records the kit
-- version a matter applied (ENG-1271) for the digest's template-effect
-- comparison.
ALTER TABLE project ADD COLUMN legal_plan_id UUID REFERENCES legal_plan (id);
ALTER TABLE project ADD COLUMN plan_member_id TEXT;
ALTER TABLE project ADD COLUMN benefit_code TEXT;
ALTER TABLE project ADD COLUMN plan_matter_type TEXT CHECK (plan_matter_type IN ('document_review', 'letter', 'uncontested_divorce', 'court_appearance'));
ALTER TABLE project ADD COLUMN plan_state TEXT;
ALTER TABLE project ADD COLUMN plan_assigned_on DATE;
ALTER TABLE project ADD COLUMN plan_accepted_on DATE;
ALTER TABLE project ADD COLUMN plan_closed_on DATE;
ALTER TABLE project ADD COLUMN kit_version TEXT;

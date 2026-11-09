-- The Plan Case Number (docs/glossary/plan-case-number.md): the number a
-- Legal Plan gave one referral — ARAG's CaseAssist number, LegalEASE's
-- member case number — verbatim. Every plan email and claim cites it, so it
-- is the key that links a plan matter back to how it is paid. NULL for a
-- matter no plan referred. The accessor
-- ([`store::projects`]`::set_plan_columns`) checks its shape.
ALTER TABLE project ADD COLUMN plan_case_number TEXT;

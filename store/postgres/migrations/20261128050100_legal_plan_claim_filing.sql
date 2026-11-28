-- Where and how a Legal Plan takes its claims (LAW-585, part of LAW-535):
-- the claims address or portal, the email subject, a separate invoice
-- address, and the proof the plan asks for. Firm data, set with
-- `navigator legal-plans claim-filing`; the claim packet reads it rather
-- than hard-coding any plan. See docs/legal-plans.md.
ALTER TABLE legal_plan ADD COLUMN claim_filing JSONB;

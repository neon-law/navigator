ALTER TABLE project ADD COLUMN plan_cap_shape TEXT;
ALTER TABLE project ADD COLUMN plan_cap_cents INTEGER;
ALTER TABLE project ADD COLUMN plan_cap_minutes INTEGER;
ALTER TABLE project ADD COLUMN plan_contested_cap_cents INTEGER;
ALTER TABLE project ADD COLUMN plan_rate_cents_per_hour INTEGER;
ALTER TABLE project ADD COLUMN plan_paralegal_rate_cents_per_hour INTEGER;

-- A recorded shape belongs to one linked plan and benefit code.
ALTER TABLE project ADD CONSTRAINT project_plan_benefit_shape_check CHECK (plan_cap_shape IN ('hourly_cap', 'dollar_cap', 'contested_uncontested', 'consult_only', 'flat_fee'));
ALTER TABLE project ADD CONSTRAINT project_plan_benefit_link_check CHECK (plan_cap_shape IS NULL OR (legal_plan_id IS NOT NULL AND benefit_code IS NOT NULL));

-- Each shape requires its corresponding cap units and keeps other caps absent.
ALTER TABLE project ADD CONSTRAINT project_plan_benefit_hourly_cap_check CHECK ((plan_cap_minutes IS NOT NULL) = COALESCE(plan_cap_shape = 'hourly_cap', FALSE) AND COALESCE(plan_cap_minutes, 1) > 0);
ALTER TABLE project ADD CONSTRAINT project_plan_benefit_dollar_cap_check CHECK ((plan_cap_cents IS NOT NULL) = COALESCE(plan_cap_shape IN ('dollar_cap', 'flat_fee', 'contested_uncontested'), FALSE) AND COALESCE(plan_cap_cents, 1) > 0);
ALTER TABLE project ADD CONSTRAINT project_plan_benefit_contested_cap_check CHECK ((plan_contested_cap_cents IS NOT NULL) = COALESCE(plan_cap_shape = 'contested_uncontested', FALSE) AND COALESCE(plan_contested_cap_cents, 1) > 0);

ALTER TABLE project ADD CONSTRAINT project_plan_benefit_rates_shape_check CHECK (plan_cap_shape IS NOT NULL OR (plan_rate_cents_per_hour IS NULL AND plan_paralegal_rate_cents_per_hour IS NULL));
ALTER TABLE project ADD CONSTRAINT project_plan_benefit_rates_check CHECK (COALESCE(plan_rate_cents_per_hour, 0) >= 0 AND COALESCE(plan_paralegal_rate_cents_per_hour, 0) >= 0);

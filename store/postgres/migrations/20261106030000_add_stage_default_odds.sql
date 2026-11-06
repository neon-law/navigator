-- A Firm's default win/settle/lose odds per Case Stage, in basis points. The
-- current stage's defaults stand in when a matter with a Contingency Term has
-- no Outcome Estimate, always labeled a default so nobody reads a Firm
-- default as a lawyer's judgment. Defaults carry odds
-- only, never a recovery (docs/glossary/case-stage.md).
ALTER TABLE stage_definition ADD COLUMN default_win_at_trial_bps INTEGER CHECK (default_win_at_trial_bps BETWEEN 0 AND 10000);
ALTER TABLE stage_definition ADD COLUMN default_settle_bps INTEGER CHECK (default_settle_bps BETWEEN 0 AND 10000);
ALTER TABLE stage_definition ADD COLUMN default_lose_bps INTEGER CHECK (default_lose_bps BETWEEN 0 AND 10000);
ALTER TABLE stage_definition ADD CONSTRAINT stage_definition_default_odds CHECK ((default_win_at_trial_bps IS NULL AND default_settle_bps IS NULL AND default_lose_bps IS NULL) OR (default_win_at_trial_bps IS NOT NULL AND default_settle_bps IS NOT NULL AND default_lose_bps IS NOT NULL AND default_win_at_trial_bps + default_settle_bps + default_lose_bps = 10000));

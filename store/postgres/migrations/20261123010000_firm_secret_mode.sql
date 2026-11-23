ALTER TABLE firm_integration_secret ADD COLUMN stripe_mode TEXT CHECK (stripe_mode IS NULL OR (provider = 'stripe' AND stripe_mode IN ('sandbox', 'live')));

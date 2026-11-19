-- The authenticated envelope includes the exact timestamp spelling, not its instant.
ALTER TABLE deployment_secret_change ADD COLUMN inserted_at_text TEXT;

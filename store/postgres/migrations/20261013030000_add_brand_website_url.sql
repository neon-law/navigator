-- A Brand's canonical website, `https://<host>` with a lowercase ASCII host.
-- Its host is the Brand's approved email domain; NULL approves none.
ALTER TABLE brand ADD COLUMN website_url TEXT CHECK (website_url ~ '^https://[a-z0-9-]+(\.[a-z0-9-]+)+$');

-- The sender-identity vocabulary grows with the inbox: the social channels
-- Navigator ingests on (Meta's three surfaces, YouTube, and TikTok) can name
-- a Person's identity the same way GitHub or Slack already do, so the inbox
-- can resolve a sender instead of showing a bare provider id.
ALTER TABLE person_external_identity DROP CONSTRAINT person_external_identity_system_check;
ALTER TABLE person_external_identity ADD CONSTRAINT person_external_identity_system_check CHECK (system IN ('github', 'slack', 'docusign', 'google', 'linear', 'claude', 'chatgpt', 'notion', 'facebook', 'instagram', 'threads', 'youtube', 'tiktok'));

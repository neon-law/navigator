-- Email ingest threads a delivery into the Conversation of a Message it names in
-- In-Reply-To or References, looked up by that Message's RFC 5322 Message-ID.
CREATE INDEX message_provider_message_id ON message (provider_message_id) WHERE provider_message_id IS NOT NULL;

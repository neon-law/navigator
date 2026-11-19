-- One Slack message per Project pull request: where the notice was posted, so
-- each later report of the same pull request edits that message in place.
CREATE TABLE project_pull_request_notice (
    project_id UUID NOT NULL,
    number INTEGER NOT NULL CHECK (number > 0),
    channel_id TEXT NOT NULL CHECK (channel_id <> ''),
    message_ts TEXT NOT NULL CHECK (message_ts <> ''),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (project_id, number)
);

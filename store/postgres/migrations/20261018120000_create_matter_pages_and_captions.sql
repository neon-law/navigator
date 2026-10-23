-- A Project's synced `pages:` entries and `matter:` block. SurrealDB remains
-- the write authority; these tables are opt-in mirrors. The compound
-- Project/Firm key proves each row belongs to its Project's Firm and follows
-- the Project when it moves.
CREATE TABLE matter_page (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    firm_id UUID NOT NULL REFERENCES firm(id),
    slug TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    draft TEXT,
    position BIGINT NOT NULL CHECK (position >= 0),
    client_sections TEXT[],
    lawyer_sections TEXT[],
    clerk_sections TEXT[],
    synced_at TIMESTAMPTZ NOT NULL,
    UNIQUE (project_id, slug) DEFERRABLE INITIALLY DEFERRED,
    UNIQUE (project_id, position) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, firm_id) REFERENCES project (id, firm_id) ON UPDATE CASCADE
);

CREATE TABLE matter_caption (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL UNIQUE REFERENCES project(id),
    firm_id UUID NOT NULL REFERENCES firm(id),
    court TEXT[] NOT NULL,
    case_number TEXT NOT NULL,
    judicial_officer TEXT NOT NULL,
    department TEXT NOT NULL,
    filed_on TEXT NOT NULL,
    trial_on TEXT,
    parties JSONB NOT NULL CHECK (jsonb_typeof(parties) = 'array'),
    counsel JSONB NOT NULL CHECK (jsonb_typeof(counsel) = 'object'),
    synced_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (project_id, firm_id) REFERENCES project (id, firm_id) ON UPDATE CASCADE
);

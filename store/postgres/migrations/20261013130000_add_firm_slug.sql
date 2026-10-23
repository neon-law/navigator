-- A Firm's slug names its Forgejo organization (`<firm-slug>/<project-code>`).
-- Lowercase letters, digits, and single hyphens, at most 40 characters, unique
-- per installation, and immutable once written.
ALTER TABLE firm ADD COLUMN slug TEXT CHECK (slug ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(slug) <= 40);

-- The derivation `store::firms::slug::derive` performs: NFKD, drop combining
-- marks and apostrophes, lowercase ASCII, one hyphen per run of anything else,
-- trimmed, cut to 40 characters without a trailing hyphen.
CREATE OR REPLACE FUNCTION firm_slug_from_name(name TEXT) RETURNS TEXT
    LANGUAGE sql IMMUTABLE STRICT
    AS $$
    SELECT rtrim(left(trim(BOTH '-' FROM regexp_replace(lower(regexp_replace(
        normalize(name, NFKD),
        '[̀-ͯ᪰-᫿᷀-᷿⃐-⃿︠-︯''’ʼ]', '', 'g')),
        '[^a-z0-9]+', '-', 'g')), 40), '-')
$$;

CREATE OR REPLACE FUNCTION firm_slug_immutable() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.slug IS DISTINCT FROM OLD.slug THEN
        RAISE EXCEPTION 'firm.slug is immutable' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

-- Backfill existing Firms. An invalid derived slug fails the CHECK and two
-- Firms deriving one slug fail the unique index: the migration stops rather
-- than suffixing.
DO $$
BEGIN
    UPDATE firm SET slug = firm_slug_from_name(name) WHERE slug IS NULL;
    ALTER TABLE firm ALTER COLUMN slug SET NOT NULL;
    CREATE TRIGGER firm_slug_immutable BEFORE UPDATE OF slug ON firm
        FOR EACH ROW EXECUTE FUNCTION firm_slug_immutable();
END $$;

CREATE UNIQUE INDEX firm_slug ON firm (slug);

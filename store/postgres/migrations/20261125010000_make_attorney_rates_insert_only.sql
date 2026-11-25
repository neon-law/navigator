-- Attorney Rate rows are the effective-dated price history for captured work.
-- Keep rows immutable, including deletes cascaded from their references.

CREATE FUNCTION reject_attorney_rate_delete() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Attorney rates are effective-dated history and cannot be deleted';
END;
$$;

CREATE TRIGGER attorney_rate_delete_immutable
    BEFORE DELETE ON attorney_rate
    FOR EACH ROW EXECUTE FUNCTION reject_attorney_rate_delete();

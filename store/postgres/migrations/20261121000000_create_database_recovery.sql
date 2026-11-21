-- A native restore retains its own creation identity and unverified source bytes.
-- Completion follows independent exact target validation behind the connection fence.
CREATE TABLE database_recovery (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
    recovery_operation UUID NOT NULL UNIQUE CHECK (recovery_operation <> '00000000-0000-0000-0000-000000000000'::uuid),
    creation_operation UUID NOT NULL REFERENCES database_origin(creation_operation),
    manifest_sha256 TEXT NOT NULL CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
    manifest_bytes BYTEA NOT NULL CHECK (octet_length(manifest_bytes) BETWEEN 1 AND 16777216),
    source_identity_bytes BYTEA NOT NULL CHECK (octet_length(source_identity_bytes) BETWEEN 1 AND 1048576),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    verified_plan_sha256 TEXT CHECK (verified_plan_sha256 ~ '^[0-9a-f]{64}$'),
    CHECK ((completed_at IS NULL) = (verified_plan_sha256 IS NULL)),
    CHECK (recovery_operation <> creation_operation)
);
CREATE FUNCTION protect_database_recovery() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND OLD.completed_at IS NULL AND NEW.completed_at IS NOT NULL AND
       NEW.verified_plan_sha256 IS NOT NULL AND
       (to_jsonb(OLD) - 'completed_at' - 'verified_plan_sha256') =
       (to_jsonb(NEW) - 'completed_at' - 'verified_plan_sha256') THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'Database Recovery intent is immutable';
END;
$$;
CREATE TRIGGER database_recovery_immutable BEFORE UPDATE OR DELETE ON database_recovery
    FOR EACH ROW EXECUTE FUNCTION protect_database_recovery();
CREATE TRIGGER database_recovery_no_truncate BEFORE TRUNCATE ON database_recovery
    FOR EACH STATEMENT EXECUTE FUNCTION protect_database_recovery();
REVOKE ALL ON database_recovery FROM PUBLIC;
CREATE FUNCTION database_recovery_pending() RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog AS $$
    WITH actual AS (
        SELECT datname::text AS name,oid::bigint AS oid,shobj_description(oid,'pg_database') AS marker
        FROM pg_catalog.pg_database WHERE datname=current_database()
    )
    SELECT CASE WHEN EXISTS(SELECT 1 FROM public.database_recovery) THEN NOT EXISTS (
        SELECT 1 FROM public.database_recovery r JOIN public.database_origin o USING(creation_operation),actual a
        WHERE r.completed_at IS NOT NULL AND r.verified_plan_sha256 IS NOT NULL
        AND o.database_name=a.name AND o.database_oid=a.oid
        AND a.marker='navigator-native-origin-v1:'||r.creation_operation::text||':recovery:'||r.recovery_operation::text
    ) ELSE EXISTS(SELECT 1 FROM actual WHERE marker LIKE 'navigator-native-origin-v1:%:recovery:%') END;
$$;
REVOKE ALL ON FUNCTION database_recovery_pending() FROM PUBLIC;

CREATE OR REPLACE FUNCTION protect_database_origin_runtime(runtime_role REGROLE) RETURNS VOID
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
DECLARE
    role_name TEXT;
BEGIN
    SELECT rolname INTO STRICT role_name FROM pg_catalog.pg_roles WHERE oid = runtime_role;
    EXECUTE format('REVOKE ALL ON TABLE public.database_origin, public.database_origin_upgrade FROM %I', role_name);
    EXECUTE format('GRANT SELECT ON TABLE public.database_origin, public.database_origin_upgrade TO %I', role_name);
    EXECUTE format('REVOKE ALL ON TABLE public.durable_object_handover, public.durable_object_ownership FROM %I', role_name);
    EXECUTE format('GRANT SELECT ON TABLE public.durable_object_handover, public.durable_object_ownership TO %I', role_name);
    EXECUTE format('GRANT EXECUTE ON FUNCTION public.database_origin_schema_contract() TO %I', role_name);
    EXECUTE format('REVOKE ALL ON TABLE public.database_recovery FROM %I', role_name);
    EXECUTE format('GRANT EXECUTE ON FUNCTION public.database_recovery_pending() TO %I', role_name);
    EXECUTE format('REVOKE ALL ON FUNCTION public.protect_database_origin_runtime(regrole) FROM %I', role_name);
END;
$$;
REVOKE ALL ON FUNCTION protect_database_origin_runtime(REGROLE) FROM PUBLIC;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'navigator_app') THEN
        PERFORM public.protect_database_origin_runtime('navigator_app'::regrole);
    END IF;
END;
$$;

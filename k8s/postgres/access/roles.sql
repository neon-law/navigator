-- Least-privilege login roles for the `navigator` database.
--
-- Idempotent: every run converges roles, ownership, and grants to this file,
-- and sets each password from its mounted Secret. Re-running it after a
-- Secret update is the rotation step. Run by the `navigator-postgres-roles`
-- Job as the bootstrap superuser, the only caller that needs that role.
--
--   navigator_owner   migration login; owns the database, schema objects and DDL
--   navigator_app     runtime login for `web`; row reads and writes only
--   navigator_backup  REPLICATION login for base backups and WAL streaming only
--   navigator_dbos    DBOS durable-execution login; owns only the `dbos` schema
--
-- The DBOS login converges only once its password Secret is mounted, so the Job
-- runs unchanged for a deployment that has not added it. It has no rights on
-- `public`, and no application login has rights on `dbos`. It holds CREATE on
-- the database because the SDK's own migration runs
-- `CREATE SCHEMA IF NOT EXISTS dbos`, and PostgreSQL checks that right before it
-- checks whether the schema exists.
-- Apache AGE is provisioned here, in this superuser init path: creating the
-- extension needs superuser, and the `navigator-migrate` Job (and every graph
-- query path) runs as a non-superuser login. Cypher() parses only in sessions
-- with AGE's library loaded, so each application login preloads it for its
-- own sessions. Forgejo's database and role are separate and
-- untouched.

\set ON_ERROR_STOP on
-- A failed statement must never echo a password into the Job log or the
-- server log.
\set VERBOSITY terse
\set SHOW_CONTEXT never
SET log_min_error_statement = panic;
SET log_statement = 'none';
SET password_encryption = 'scram-sha-256';

\set owner_password `cat /etc/navigator/postgres/migrator/password`
\set app_password `cat /etc/navigator/postgres/app/password`
\set backup_password `cat /etc/navigator/postgres/backup/password`

SELECT format('CREATE ROLE %I', role)
FROM unnest(ARRAY['navigator_owner', 'navigator_app', 'navigator_backup']) AS role
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role)
\gexec

ALTER ROLE navigator_owner WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS
  CONNECTION LIMIT 5 PASSWORD :'owner_password';
ALTER ROLE navigator_app WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS
  CONNECTION LIMIT 40 PASSWORD :'app_password';
ALTER ROLE navigator_backup WITH LOGIN REPLICATION NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS
  CONNECTION LIMIT 4 PASSWORD :'backup_password';

-- The migration login owns the database, so PostgreSQL 15+'s
-- `pg_database_owner` hands it the `public` schema. Nobody else may connect
-- or create temporary tables unless granted below.
ALTER DATABASE navigator OWNER TO navigator_owner;
REVOKE ALL ON DATABASE navigator FROM PUBLIC;
GRANT CONNECT ON DATABASE navigator TO navigator_app;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO navigator_app;

-- The AGE relationship graph: the extension's `ag_catalog` is readable by the
-- migration and runtime logins; graph DDL and the loader helpers stay with the
-- migration login (the graph migration owns them, and execute on the loaders
-- is revoked from PUBLIC). AGE's parser hook must be in the library preload of
-- any session that parses Cypher, so both logins preload it per session.
CREATE EXTENSION IF NOT EXISTS age;
GRANT USAGE ON SCHEMA ag_catalog TO navigator_owner, navigator_app;
-- The graph migration's idempotence guard reads ag_graph; AGE's own runtime
-- catalog access is in-process and needs no SQL grants.
GRANT SELECT ON ALL TABLES IN SCHEMA ag_catalog TO navigator_owner, navigator_app;
ALTER ROLE navigator_owner IN DATABASE navigator SET session_preload_libraries = 'age';
ALTER ROLE navigator_app IN DATABASE navigator SET session_preload_libraries = 'age';

-- Objects created before these roles existed (by the bootstrap superuser)
-- move to the migration login. Extension members stay with their extension,
-- and owned sequences follow their table.
SELECT format('ALTER TABLE %s OWNER TO navigator_owner', c.oid::regclass)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'public'
  AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S')
  AND c.relowner <> 'navigator_owner'::regrole
  AND NOT EXISTS (
    SELECT FROM pg_depend AS d
    WHERE d.classid = 'pg_class'::regclass
      AND d.objid = c.oid
      AND d.deptype IN ('e', 'a', 'i')
  )
\gexec

SELECT format('ALTER ROUTINE %s OWNER TO navigator_owner', p.oid::regprocedure)
FROM pg_proc AS p
JOIN pg_namespace AS n ON n.oid = p.pronamespace
WHERE n.nspname = 'public'
  AND p.proowner <> 'navigator_owner'::regrole
  AND NOT EXISTS (
    SELECT FROM pg_depend AS d
    WHERE d.classid = 'pg_proc'::regclass
      AND d.objid = p.oid
      AND d.deptype = 'e'
  )
\gexec

-- Runtime rows: read and write, never DDL, TRUNCATE, or REFERENCES. Append-only
-- tables keep their own refusal triggers.
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO navigator_app;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO navigator_app;
ALTER DEFAULT PRIVILEGES FOR ROLE navigator_owner IN SCHEMA public
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO navigator_app;
ALTER DEFAULT PRIVILEGES FOR ROLE navigator_owner IN SCHEMA public
  GRANT USAGE, SELECT ON SEQUENCES TO navigator_app;

-- Migration bookkeeping belongs to the migration login alone.
SELECT format('REVOKE ALL ON TABLE %I FROM navigator_app', relname)
FROM pg_class
WHERE relnamespace = 'public'::regnamespace
  AND relname IN ('_sqlx_migrations', 'schema_migrations')
\gexec

-- The DBOS login, when its Secret is mounted. `optional: true` on the volume
-- leaves the file absent otherwise, and an empty password skips this block.
\set dbos_password `cat /etc/navigator/postgres/dbos/password 2>/dev/null`
SELECT :'dbos_password' <> '' AS dbos_login \gset
\if :dbos_login
SELECT 'CREATE ROLE navigator_dbos'
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'navigator_dbos')
\gexec
ALTER ROLE navigator_dbos WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS
  CONNECTION LIMIT 20 PASSWORD :'dbos_password';
GRANT CONNECT, CREATE ON DATABASE navigator TO navigator_dbos;
CREATE SCHEMA IF NOT EXISTS dbos AUTHORIZATION navigator_dbos;
ALTER SCHEMA dbos OWNER TO navigator_dbos;
REVOKE ALL ON SCHEMA dbos FROM PUBLIC;
REVOKE ALL ON SCHEMA dbos FROM navigator_owner, navigator_app, navigator_backup;
REVOKE ALL ON SCHEMA public FROM navigator_dbos;
\endif

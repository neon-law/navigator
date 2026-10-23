-- Forgejo's login and database on the installation's shared PostgreSQL.
--
-- Idempotent: every run converges the `forgejo` login, its database, and the
-- database's grants to this file, and sets the password from its mounted
-- Secret. Re-running it after a Secret update is the rotation step. Run by the
-- `navigator-forgejo-database` Job as the bootstrap superuser.
--
-- The login owns only its own database, so Forgejo runs its own schema
-- migrations there. It is never a superuser, never creates roles or
-- databases, and is granted nothing on the `navigator` database; Navigator's
-- `roles.sql` already revokes PUBLIC's rights there.

\set ON_ERROR_STOP on
-- A failed statement must never echo a password into the Job or server log.
\set VERBOSITY terse
\set SHOW_CONTEXT never
SET log_min_error_statement = panic;
SET log_statement = 'none';
SET password_encryption = 'scram-sha-256';

\set forgejo_password `cat /etc/navigator/forgejo/database/password`

SELECT 'CREATE ROLE forgejo'
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'forgejo')
\gexec

ALTER ROLE forgejo WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS
  CONNECTION LIMIT 20 PASSWORD :'forgejo_password';

SELECT 'CREATE DATABASE forgejo OWNER forgejo TEMPLATE template0 ENCODING ''UTF8'''
WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'forgejo')
\gexec

ALTER DATABASE forgejo OWNER TO forgejo;
REVOKE ALL ON DATABASE forgejo FROM PUBLIC;
GRANT CONNECT, TEMPORARY ON DATABASE forgejo TO forgejo;
REVOKE ALL ON DATABASE navigator FROM forgejo;

//! Coordinate a rename across two systems. PostgreSQL commits the intent before
//! the provider call. Either canonical Forgejo state can then resume the move.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use reqwest::{redirect::Policy, Client, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::FirmNamespaceMove;
use store::postgres::PostgresDb;

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct Binding {
    suffix: String,
    repository_id: i64,
}

type Bindings = BTreeMap<Uuid, Binding>;

struct Forge {
    origin: String,
    client: Client,
    token: String,
}

impl Forge {
    async fn get(&self, path: &str) -> Result<Option<serde_json::Value>> {
        let response = self
            .client
            .get(format!("{}/api/v1/{path}", self.origin))
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status() == StatusCode::OK,
            "Forgejo lookup returned {}",
            response.status()
        );
        Ok(Some(response.json().await?))
    }

    async fn organization(&self, slug: &str) -> Result<Option<i64>> {
        let Some(org) = self.get(&format!("orgs/{slug}")).await? else {
            return Ok(None);
        };
        ensure!(
            org["name"] == slug || org["username"] == slug,
            "Forgejo did not return the canonical namespace"
        );
        Ok(Some(
            org["id"]
                .as_i64()
                .context("missing Forgejo organization ID")?,
        ))
    }

    async fn bindings(
        &self,
        paths: BTreeMap<Uuid, String>,
        org: &str,
        owner: i64,
    ) -> Result<Bindings> {
        let mut result = Bindings::new();
        for (project, suffix) in paths {
            let path = suffix.strip_suffix('/').unwrap_or(&suffix);
            let name = path.strip_suffix(".git").unwrap_or(path);
            ensure!(
                cloud::workspace::is_valid_slug(name),
                "unexpected bound repository path"
            );
            let path = format!("{org}/{name}");
            let repo = self
                .get(&format!("repos/{path}"))
                .await?
                .context("bound repository unavailable")?;
            ensure!(
                repo["full_name"] == path && repo["owner"]["id"] == owner,
                "repository owner or canonical name differs"
            );
            let repository_id = repo["id"]
                .as_i64()
                .filter(|id| *id > 0)
                .context("missing repository ID")?;
            result.insert(
                project,
                Binding {
                    suffix,
                    repository_id,
                },
            );
        }
        Ok(result)
    }

    async fn verify_old_path(&self, args: &FirmNamespaceMove) -> Result<()> {
        let source = format!("{}/api/v1/orgs/{}", self.origin, args.from);
        let response = self
            .client
            .get(&source)
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(()),
            StatusCode::OK => {
                let org: serde_json::Value = response.json().await?;
                ensure!(
                    org["id"] == args.organization_id
                        && (org["name"] == args.to || org["username"] == args.to),
                    "old organization name is occupied"
                );
                Ok(())
            }
            status if status.is_redirection() => {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .context("missing redirect")?
                    .to_str()?;
                let redirect = url::Url::parse(&source)?.join(location)?;
                ensure!(
                    redirect.as_str() == format!("{}/api/v1/orgs/{}", self.origin, args.to),
                    "unexpected old namespace redirect"
                );
                Ok(())
            }
            status => anyhow::bail!("old namespace check returned {status}"),
        }
    }

    async fn rename(&self, args: &FirmNamespaceMove) -> Result<()> {
        let response = self
            .client
            .post(format!("{}/api/v1/orgs/{}/rename", self.origin, args.from))
            .header("Authorization", format!("token {}", self.token))
            .json(&serde_json::json!({"new_name": args.to}))
            .send()
            .await?;
        ensure!(
            response.status() == StatusCode::NO_CONTENT,
            "Forgejo rename returned {}",
            response.status()
        );
        Ok(())
    }
}

async fn paths(db: &mut PgConnection, firm: Uuid, prefix: &str) -> Result<BTreeMap<Uuid, String>> {
    let rows = sqlx::query(
        "SELECT id, firm_id, repository_url FROM project WHERE starts_with(lower(repository_url), lower($1))",
    )
    .bind(prefix)
    .fetch_all(db)
    .await?;
    let mut paths = BTreeMap::new();
    for row in rows {
        ensure!(
            row.try_get::<Option<Uuid>, _>("firm_id")? == Some(firm),
            "another Firm has a binding in the namespace"
        );
        let url: String = row.try_get("repository_url")?;
        paths.insert(
            row.try_get("id")?,
            url.strip_prefix(prefix)
                .context("repository prefix changed")?
                .to_string(),
        );
    }
    Ok(paths)
}

async fn validate_database(
    db: &mut PgConnection,
    args: &FirmNamespaceMove,
    slug: &str,
) -> Result<()> {
    let (owner, conflict): (bool, bool) = sqlx::query_as(
        "SELECT pg_has_role(current_user, (SELECT relowner FROM pg_class WHERE oid='firm'::regclass), 'USAGE'),
                EXISTS (SELECT FROM firm WHERE slug=$1 AND id<>$2)
                OR EXISTS (SELECT FROM firm_forgejo_organization
                           WHERE (firm_id=$2 AND (organization<>$3 OR forgejo_id<>$4))
                              OR (firm_id<>$2 AND (organization IN ($1,$3) OR forgejo_id=$4)))")
        .bind(&args.to).bind(args.firm).bind(slug).bind(args.organization_id).fetch_one(db).await?;
    ensure!(owner, "use the installation's PostgreSQL migration role");
    ensure!(!conflict, "conflicting Firm or provisioning record");
    Ok(())
}

async fn coordinate(pg: &PostgresDb, forge: &Forge, args: &FirmNamespaceMove) -> Result<()> {
    let old = format!("{}/{}/", forge.origin, args.from);
    let new = format!("{}/{}/", forge.origin, args.to);
    let mut tx = pg.pool().begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await?;
    let slug: String = sqlx::query_scalar("SELECT slug FROM firm WHERE id=$1")
        .bind(args.firm)
        .fetch_one(&mut *tx)
        .await?;
    let receipt = sqlx::query("SELECT id, old_slug,new_slug,forgejo_id,forge_origin,repositories,completed_at IS NOT NULL AS complete FROM firm_namespace_move WHERE firm_id=$1 ORDER BY inserted_at DESC LIMIT 1")
        .bind(args.firm).fetch_optional(&mut *tx).await?;
    let receipt = receipt.filter(|row| !row.get::<bool, _>("complete") || slug == args.to);
    if let Some(row) = &receipt {
        ensure!(
            row.try_get::<String, _>("old_slug")? == args.from
                && row.try_get::<String, _>("new_slug")? == args.to
                && row.try_get::<String, _>("forge_origin")? == forge.origin
                && row.try_get::<i64, _>("forgejo_id")? == args.organization_id,
            "existing move is bound to different coordinates"
        );
    }
    let complete = receipt
        .as_ref()
        .is_some_and(|r| r.get::<bool, _>("complete"));
    ensure!(
        slug == if complete { &args.to } else { &args.from }.as_str(),
        "Firm slug differs from move"
    );
    validate_database(&mut tx, args, &slug).await?;
    let old_paths = paths(&mut tx, args.firm, &old).await?;
    let new_paths = paths(&mut tx, args.firm, &new).await?;
    if receipt.is_none() && old_paths.is_empty() {
        let recorded: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT FROM firm_forgejo_organization WHERE firm_id=$1 AND forgejo_id=$2 AND organization=$3)")
            .bind(args.firm).bind(args.organization_id).bind(&args.from).fetch_one(&mut *tx).await?;
        ensure!(recorded, "Firm has no binding to this Forgejo organization");
    }
    let target = forge.organization(&args.to).await?;
    let at_target = target == Some(args.organization_id);
    ensure!(
        target.is_none() || (at_target && receipt.is_some()),
        "destination organization already exists"
    );
    if at_target {
        forge.verify_old_path(args).await?;
    } else {
        ensure!(
            forge.organization(&args.from).await? == Some(args.organization_id),
            "source organization ID differs"
        );
        ensure!(
            forge.get(&format!("users/{}", args.to)).await?.is_none(),
            "destination account exists"
        );
    }
    let actual = if at_target { &args.to } else { &args.from };
    let user = forge
        .get("user")
        .await?
        .context("operator identity unavailable")?;
    let login = user["login"].as_str().context("operator login missing")?;
    let permission = forge
        .get(&format!("users/{login}/orgs/{actual}/permissions"))
        .await?
        .context("owner access unavailable")?;
    ensure!(permission["is_owner"] == true, "Forgejo owner required");
    if complete {
        ensure!(
            at_target && old_paths.is_empty(),
            "completed move has drifted"
        );
    } else {
        ensure!(
            new_paths.is_empty(),
            "destination already has bound Projects"
        );
    }
    let current = if complete { new_paths } else { old_paths };
    let bindings = forge
        .bindings(current, actual, args.organization_id)
        .await?;
    let id = if let Some(row) = receipt {
        let recorded: sqlx::types::Json<Bindings> = row.try_get("repositories")?;
        ensure!(
            recorded.iter().all(|(p, b)| bindings.get(p) == Some(b))
                && (complete || recorded.len() == bindings.len()),
            "bound repository identity or path has changed"
        );
        row.try_get::<Uuid, _>("id")?
    } else if args.apply {
        sqlx::query_scalar("INSERT INTO firm_namespace_move (firm_id,forge_origin,forgejo_id,old_slug,new_slug,repositories) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id")
            .bind(args.firm).bind(&forge.origin).bind(args.organization_id).bind(&args.from).bind(&args.to)
            .bind(sqlx::types::Json(&bindings)).fetch_one(&mut *tx).await?
    } else {
        Uuid::nil()
    };
    println!(
        "Verified Forgejo organization and {} Project bindings",
        bindings.len()
    );
    tx.commit().await?;
    if complete || !args.apply {
        return Ok(());
    }

    let mut tx = pg.pool().begin().await?;
    sqlx::query("SET LOCAL lock_timeout='5s'")
        .execute(&mut *tx)
        .await?;
    let pending: bool = sqlx::query_scalar(
        "SELECT completed_at IS NULL FROM firm_namespace_move WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    ensure!(
        pending,
        "another operator completed the move; run preview to verify"
    );
    if !at_target {
        ensure!(
            forge.organization(&args.from).await? == Some(args.organization_id),
            "source changed before rename"
        );
        forge.rename(args).await?;
    }
    ensure!(
        forge.organization(&args.to).await? == Some(args.organization_id),
        "destination identity differs"
    );
    forge.verify_old_path(args).await?;
    ensure!(
        forge
            .bindings(
                bindings
                    .iter()
                    .map(|(project, binding)| (*project, binding.suffix.clone()))
                    .collect(),
                &args.to,
                args.organization_id
            )
            .await?
            == bindings,
        "repository identity changed after rename"
    );

    sqlx::query("LOCK TABLE firm, firm_forgejo_organization, project IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let source_current: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT FROM firm WHERE id=$1 AND slug=$2)")
            .bind(args.firm)
            .bind(&args.from)
            .fetch_one(&mut *tx)
            .await?;
    ensure!(source_current, "Firm changed before commit");
    validate_database(&mut tx, args, &args.from).await?;
    let original = paths(&mut tx, args.firm, &old).await?;
    ensure!(
        original.len() == bindings.len()
            && original
                .iter()
                .all(|(k, v)| bindings.get(k).is_some_and(|b| &b.suffix == v))
            && paths(&mut tx, args.firm, &new).await?.is_empty(),
        "Project bindings changed after preflight"
    );
    sqlx::query("SELECT set_config('navigator.firm_namespace_move',$1,true)")
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
    let firm = sqlx::query("UPDATE firm SET slug=$1,updated_at=now() WHERE id=$2 AND slug=$3")
        .bind(&args.to)
        .bind(args.firm)
        .bind(&args.from)
        .execute(&mut *tx)
        .await?;
    ensure!(firm.rows_affected() == 1, "Firm changed before commit");
    sqlx::query(
        "UPDATE firm_forgejo_organization SET organization=$1,reconciled_at=now()
                     WHERE firm_id=$2 AND organization=$3 AND forgejo_id=$4",
    )
    .bind(&args.to)
    .bind(args.firm)
    .bind(&args.from)
    .bind(args.organization_id)
    .execute(&mut *tx)
    .await?;
    let changed = sqlx::query("UPDATE project SET repository_url=$1 || substr(repository_url,length($2)+1),updated_at=now() WHERE firm_id=$3 AND starts_with(repository_url,$2)")
        .bind(&new).bind(&old).bind(args.firm).execute(&mut *tx).await?;
    ensure!(
        changed.rows_affected() == bindings.len() as u64,
        "updated binding count differs"
    );
    sqlx::query("UPDATE firm_namespace_move SET completed_at=now() WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    println!("Committed namespace move {id}");
    Ok(())
}

pub(in crate::devx) fn run(args: &FirmNamespaceMove) -> Result<()> {
    ensure!(
        store::firms::slug::is_valid(&args.from)
            && store::firms::slug::is_valid(&args.to)
            && args.from != args.to
            && args.organization_id > 0,
        "invalid namespace coordinates"
    );
    let root = url::Url::parse(&std::env::var("NAVIGATOR_FORGEJO_ROOT_URL")?)?;
    ensure!(
        root.scheme() == "https"
            && root.path() == "/"
            && root.query().is_none()
            && root.fragment().is_none()
            && root.username().is_empty()
            && root.password().is_none(),
        "expected the installation HTTPS origin"
    );
    let token = if args.token_stdin {
        let stdin = std::io::stdin();
        ensure!(
            !stdin.is_terminal(),
            "pipe the Forgejo credential from the credential store"
        );
        let mut token = String::new();
        stdin.take(4097).read_to_string(&mut token)?;
        ensure!(token.len() <= 4096, "Forgejo credential input is too long");
        token.trim().to_owned()
    } else {
        std::env::var("FORGEJO_TOKEN")?
    };
    ensure!(!token.trim().is_empty(), "empty Forgejo credential");
    let forge = Forge {
        origin: root.origin().ascii_serialization(),
        client: Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(15))
            .build()?,
        token,
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async { coordinate(&PostgresDb::from_env().await?, &forge, args).await })
}

#[cfg(test)]
mod tests;

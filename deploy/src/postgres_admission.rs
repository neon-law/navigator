//! Read-only release admission from the PostgreSQL application's completed history.

use std::process::ExitCode;

use store::postgres::{PostgresDb, PostgresError};

#[cfg(test)]
async fn verify() -> Result<Vec<&'static str>, PostgresError> {
    verify_target(None).await
}

async fn verify_target(database: Option<&str>) -> Result<Vec<&'static str>, PostgresError> {
    let pg = PostgresDb::application().await?;
    if let Some(expected) = database {
        let actual: String = sqlx::query_scalar("SELECT current_database()::text")
            .fetch_one(pg.pool())
            .await
            .map_err(|_| PostgresError::History)?;
        if actual != expected {
            return Err(PostgresError::History);
        }
    }
    Ok(store::postgres::history::Family::all()
        .iter()
        .map(|family| family.scope())
        .collect())
}

fn native_coordinates_match(
    target: &crate::devx::NativeAdmissionTarget,
    get: impl Fn(&str) -> Option<String>,
) -> bool {
    get("NAVIGATOR_POSTGRES_HOST").as_deref() == Some("127.0.0.1")
        && get("NAVIGATOR_POSTGRES_PORT").and_then(|p| p.parse::<u16>().ok()) == Some(target.port)
        && get("NAVIGATOR_POSTGRES_DATABASE").as_deref() == Some(target.database.as_str())
        && get("NAVIGATOR_POSTGRES_SSLMODE").as_deref() == Some("verify-full")
        && get("NAVIGATOR_POSTGRES_ROOT_CERT")
            .and_then(|path| std::fs::canonicalize(path).ok())
            .zip(std::fs::canonicalize(&target.root_cert).ok())
            .is_some_and(|(actual, expected)| actual == expected)
}

pub fn run(json: bool) -> ExitCode {
    // This verifier builds its own runtime. It must finish before admission's runtime exists.
    let native = match crate::devx::verified_native_admission_target() {
        Ok(Some(target)) if native_coordinates_match(&target, |key| std::env::var(key).ok()) => {
            Some(target)
        }
        Ok(None) => None,
        Ok(Some(_)) | Err(_) => {
            eprintln!("hudson: native PostgreSQL coordinates or private tier verification failed");
            return ExitCode::from(2);
        }
    };
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "could not start PostgreSQL admission runtime".to_owned())
        .and_then(|runtime| {
            runtime
                .block_on(verify_target(
                    native.as_ref().map(|target| target.database.as_str()),
                ))
                .map_err(|error| error.to_string())
        });
    match result {
        Ok(scopes) => {
            if json {
                let mut report = serde_json::json!({"ready":true,"scopes":scopes});
                if native.is_some() {
                    report["native_kind_verified"] = true.into();
                }
                println!("{report}");
            } else {
                println!("PostgreSQL application history is ready");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("hudson: PostgreSQL application history is unavailable: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn admission_requires_every_current_scope_without_source_access() {
        store::test_support::with_captured_postgres(async {
            assert!(verify().await.is_ok());
            let pg = PostgresDb::from_env().await.unwrap();
            assert!(!store::postgres::native_origin::admits(&pg).await.unwrap());
            sqlx::query("DELETE FROM mirror_backfill_checkpoint WHERE run_id IN (SELECT id FROM mirror_backfill_run WHERE scope='application:1')")
                .execute(pg.pool()).await.unwrap();
            assert!(matches!(verify().await, Err(PostgresError::History)));
        }).await;
    }

    #[test]
    fn native_coordinates_bind_endpoint_database_and_private_trust_anchor() {
        let directory = tempfile::tempdir().unwrap();
        let ca = directory.path().join("ca.crt");
        let foreign = directory.path().join("foreign.crt");
        std::fs::write(&ca, "synthetic public test CA").unwrap();
        std::fs::write(&foreign, "synthetic public test CA").unwrap();
        let target = crate::devx::NativeAdmissionTarget {
            port: 21501,
            database: "navigator_native_synthetic".into(),
            root_cert: ca.clone(),
        };
        let coordinates = std::collections::BTreeMap::from([
            ("NAVIGATOR_POSTGRES_HOST", "127.0.0.1".to_owned()),
            ("NAVIGATOR_POSTGRES_PORT", target.port.to_string()),
            ("NAVIGATOR_POSTGRES_DATABASE", target.database.clone()),
            ("NAVIGATOR_POSTGRES_SSLMODE", "verify-full".to_owned()),
            (
                "NAVIGATOR_POSTGRES_ROOT_CERT",
                ca.to_string_lossy().into_owned(),
            ),
        ]);
        assert!(native_coordinates_match(&target, |key| coordinates
            .get(key)
            .cloned()));
        for key in coordinates.keys() {
            let mut absent = coordinates.clone();
            absent.remove(key);
            assert!(!native_coordinates_match(&target, |key| absent
                .get(key)
                .cloned()));
        }
        for (key, value) in [
            ("NAVIGATOR_POSTGRES_HOST", "foreign.example.test".to_owned()),
            ("NAVIGATOR_POSTGRES_PORT", "21502".to_owned()),
            ("NAVIGATOR_POSTGRES_DATABASE", "another_database".to_owned()),
            ("NAVIGATOR_POSTGRES_SSLMODE", "disable".to_owned()),
            (
                "NAVIGATOR_POSTGRES_ROOT_CERT",
                foreign.to_string_lossy().into_owned(),
            ),
        ] {
            let mut wrong = coordinates.clone();
            wrong.insert(key, value);
            assert!(!native_coordinates_match(&target, |key| wrong
                .get(key)
                .cloned()));
        }
    }

    #[tokio::test]
    async fn selected_application_database_must_match_the_verified_target() {
        store::test_support::with_postgres(async {
            let pg = PostgresDb::application().await.unwrap();
            let name: String = sqlx::query_scalar("SELECT current_database()::text")
                .fetch_one(pg.pool())
                .await
                .unwrap();
            assert!(verify_target(Some(&name)).await.is_ok());
            assert!(matches!(
                verify_target(Some("another_database")).await,
                Err(PostgresError::History)
            ));
        })
        .await;
    }
}

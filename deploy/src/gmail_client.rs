//! Operator-only Secret Manager provisioning for Navigator's local OAuth client.
use anyhow::{anyhow, Result};
use std::time::Duration;
const CLIENT_SECRET_SERVICE: &str = cli::mail::CLIENT_SECRET_SERVICE;
const PROVISIONING_DOC: &str = "docs/mail-cli.md#provision-gmail-for-navigator-mail";
struct GcpSecret {
    project: String,
    secret: String,
}

pub async fn provision(project: String, secret: String, client_id: &str) -> Result<()> {
    for value in [&project, &secret] {
        if value.is_empty()
            || value.starts_with('-')
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        {
            return Err(anyhow!(
                "GCP project and secret must be nonempty resource identifiers"
            ));
        }
    }
    let source = GcpSecret { project, secret };
    store_gcp_client_secret(client_id, fetch_gcp_secret(&source), |_, account, value| {
        cli::mail::provision_oauth_client_secret(account, value)
    })
    .await?;
    println!("OAuth client secret provisioned in Navigator's Keychain storage.");
    Ok(())
}

async fn fetch_gcp_secret(source: &GcpSecret) -> Result<String> {
    fetch_gcp_secret_with(source, std::ffi::OsStr::new("gcloud")).await
}

async fn fetch_gcp_secret_with(source: &GcpSecret, program: &std::ffi::OsStr) -> Result<String> {
    fetch_gcp_secret_with_timeout(source, program, Duration::from_secs(60)).await
}

async fn fetch_gcp_secret_with_timeout(
    source: &GcpSecret,
    program: &std::ffi::OsStr,
    timeout: Duration,
) -> Result<String> {
    let output = tokio::time::timeout(
        timeout,
        tokio::process::Command::new(program)
            .args([
                "secrets",
                "versions",
                "access",
                "latest",
                "--quiet",
                "--verbosity=error",
                "--project",
                &source.project,
                "--secret",
                &source.secret,
            ])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow!("GCP client secret lookup timed out"))?
    .map_err(|_| anyhow!("cannot run gcloud; install it and run gcloud auth login"))?;
    gcp_secret_result(output.status.success(), &output.stdout, &output.stderr)
}

fn gcp_secret_result(success: bool, stdout: &[u8], stderr: &[u8]) -> Result<String> {
    if !success {
        let diagnostic = String::from_utf8_lossy(stderr);
        let message = if diagnostic.contains("PERMISSION_DENIED") || diagnostic.contains("403") {
            "GCP client secret access denied; check membership in the lawyers group and its Secret Manager permissions"
        } else if diagnostic.contains("NOT_FOUND") || diagnostic.contains("404") {
            "GCP client secret not found; check the configured project, secret and latest version"
        } else {
            "GCP client secret lookup failed; check gcloud authentication and Secret Manager access"
        };
        return Err(anyhow!("{message} (see {PROVISIONING_DOC})"));
    }
    let secret = std::str::from_utf8(stdout)
        .map_err(|_| anyhow!("GCP client secret is not UTF-8"))?
        .trim();
    if secret.is_empty() {
        return Err(anyhow!("GCP client secret is empty; nothing was stored"));
    }
    Ok(secret.to_owned())
}

async fn store_gcp_client_secret(
    client_id: &str,
    fetch: impl std::future::Future<Output = Result<String>>,
    store: impl FnOnce(&str, &str, &str) -> Result<()>,
) -> Result<String> {
    let secret = fetch.await?;
    store(CLIENT_SECRET_SERVICE, client_id, &secret)?;
    Ok(secret)
}

#[cfg(test)]
mod gcp_tests {
    use super::*;

    #[test]
    fn gcp_secret_errors_distinguish_denied_and_missing_without_provider_output() {
        for (message, expected) in [
            ("PERMISSION_DENIED private-marker", "lawyers group"),
            ("NOT_FOUND private-marker", "not found"),
            ("unknown private-marker", "failed"),
        ] {
            let error = gcp_secret_result(false, b"private-secret", message.as_bytes())
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
            assert!(
                error.contains("docs/mail-cli.md#provision-gmail-for-navigator-mail"),
                "{error}"
            );
            assert!(!error.contains("private-marker"));
            assert!(!error.contains("private-secret"));
        }
        assert_eq!(
            gcp_secret_result(true, b" synthetic-secret\n", b"").unwrap(),
            "synthetic-secret"
        );
        assert!(gcp_secret_result(true, b"  ", b"").is_err());
        assert!(gcp_secret_result(true, &[255], b"").is_err());
    }
    #[tokio::test]
    async fn gcp_lookup_captures_bytes_and_stores_them_before_login() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("fake-gcloud");
        std::fs::write(&program, r#"#!/bin/sh
[ "$*" = "secrets versions access latest --quiet --verbosity=error --project example-mail --secret shared-client" ] || exit 2
printf 'synthetic-secret\n'
"#).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = GcpSecret {
            project: "example-mail".into(),
            secret: "shared-client".into(),
        };
        let secret = store_gcp_client_secret(
            "test-client",
            fetch_gcp_secret_with(&source, program.as_os_str()),
            |service, account, bytes| {
                assert_eq!(service, CLIENT_SECRET_SERVICE);
                assert_eq!(account, "test-client");
                assert_eq!(bytes, "synthetic-secret");
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(secret, "synthetic-secret");
        assert!(store_gcp_client_secret(
            "test-client",
            async { Err(anyhow!("denied")) },
            |_, _, _| panic!("a failed fetch must not write Keychain")
        )
        .await
        .is_err());
        assert!(store_gcp_client_secret(
            "test-client",
            async { Ok("synthetic".into()) },
            |_, _, _| Err(anyhow!("Keychain unavailable"))
        )
        .await
        .is_err());
        assert!(
            fetch_gcp_secret_with(&source, dir.path().join("absent").as_os_str())
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn gcp_process_failures_do_not_escape_into_errors() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("fake-gcloud");
        std::fs::write(&program, "#!/bin/sh\nprintf 'private-stdout'\nprintf 'PERMISSION_DENIED private-stderr' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = GcpSecret {
            project: "example-mail".into(),
            secret: "shared-client".into(),
        };
        let error = fetch_gcp_secret_with(&source, program.as_os_str())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("lawyers group"));
        assert!(!error.contains("private-stdout"));
        assert!(!error.contains("private-stderr"));
    }
    #[tokio::test]
    async fn gcp_timeout_kills_the_child_and_never_writes_keychain() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("waiting-gcloud");
        std::fs::write(&program, "#!/bin/sh\necho $$ > \"$0.pid\"\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = GcpSecret {
            project: "example-mail".into(),
            secret: "shared-client".into(),
        };
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            store_gcp_client_secret(
                "test-client",
                fetch_gcp_secret_with_timeout(&source, program.as_os_str(), Duration::from_secs(1)),
                |_, _, _| panic!("a timed-out lookup must not write Keychain"),
            ),
        )
        .await
        .expect("lookup must respect its timeout")
        .unwrap_err();
        assert_eq!(error.to_string(), "GCP client secret lookup timed out");
        let pid = std::fs::read_to_string(program.with_extension("pid")).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let status = tokio::process::Command::new("/bin/kill")
                    .args(["-0", pid.trim()])
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await
                    .unwrap();
                if !status.success() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the timed-out child must exit and be reaped");
    }
}

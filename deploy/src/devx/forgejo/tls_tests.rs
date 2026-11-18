use super::*;
use sqlx::{Connection, Row};

fn write_material(directory: &Path, material: &ServerCertificate) {
    for (name, value) in [
        ("ca.crt", &material.ca),
        ("server.crt", &material.certificate),
        ("server.key", &material.key),
    ] {
        std::fs::write(directory.join(name), value).unwrap();
    }
}

#[test]
fn postgres_tls_issues_a_distinct_constrained_root_and_server() {
    let material = server_certificate("navigator").unwrap();
    assert_ne!(material.ca, material.certificate);
    let directory = tempfile::tempdir().unwrap();
    write_material(directory.path(), &material);
    for (name, constraint) in [("ca.crt", "CA:TRUE"), ("server.crt", "CA:FALSE")] {
        let output = Command::new("openssl")
            .args(["x509", "-noout", "-text", "-in"])
            .arg(directory.path().join(name))
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(constraint));
        if name == "server.crt" {
            assert!(text.contains("TLS Web Server Authentication"));
            assert!(text.contains("DNS:navigator-postgres.navigator.svc.cluster.local"));
            assert!(text.contains("IP Address:127.0.0.1"));
        }
    }
}

struct ProbeContainer(String);
impl Drop for ProbeContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "--force", &self.0])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn probe_image() -> String {
    let document: serde_yaml::Value =
        serde_yaml::from_str(include_str!("../../../../k8s/postgres/kustomization.yaml")).unwrap();
    let image = &document["images"][0];
    format!(
        "{}@{}",
        image["newName"].as_str().unwrap(),
        image["digest"].as_str().unwrap()
    )
}

fn linux_material(directory: &Path, image: &str) -> ServerCertificate {
    use std::os::unix::fs::PermissionsExt;
    let wrapper = directory.join("openssl-linux");
    // Every command uses the production issuer; only its executable runs in
    // the pinned Linux image. Mount precisely this private scratch directory.
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    std::fs::write(&wrapper, format!(
        "#!/bin/sh\nexec docker run --rm --network none --user {}:{} --mount {} --entrypoint openssl {} \"$@\"\n",
        Command::new("id").arg("-u").output().map(|o| String::from_utf8(o.stdout).unwrap().trim().to_owned()).unwrap(),
        Command::new("id").arg("-g").output().map(|o| String::from_utf8(o.stdout).unwrap().trim().to_owned()).unwrap(),
        quote(&format!("type=bind,source={},target={}", directory.display(), directory.display())), quote(image)
    )).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    issue_server_certificate("navigator", directory, &wrapper).unwrap()
}

fn start_probe(directory: &Path, image: &str) -> (ProbeContainer, u16) {
    let container = ProbeContainer(format!(
        "navigator-pg-tls-proof-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let script = "install -d -o postgres -g postgres -m 700 /tmp/probe-tls; install -o postgres -g postgres -m 600 /fixture/server.key /tmp/probe-tls/server.key; install -m 644 /fixture/server.crt /tmp/probe-tls/server.crt; exec /usr/local/bin/docker-entrypoint.sh postgres -c ssl=on -c ssl_cert_file=/tmp/probe-tls/server.crt -c ssl_key_file=/tmp/probe-tls/server.key";
    let output = Command::new("docker")
        .args([
            "run",
            "--detach",
            "--name",
            &container.0,
            "--network",
            "bridge",
            "-p",
            "127.0.0.1::5432",
            "--tmpfs",
            "/var/lib/postgresql/data:rw,size=512m",
            "-e",
            "POSTGRES_PASSWORD=synthetic",
            "-e",
            "POSTGRES_DB=smoke",
            "--mount",
        ])
        .arg(format!(
            "type=bind,source={},target=/fixture,readonly",
            directory.display()
        ))
        .args(["--entrypoint", "bash", image, "-ceu", script])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new("docker")
        .args(["inspect", &container.0])
        .output()
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let binding = &info[0]["NetworkSettings"]["Ports"]["5432/tcp"][0];
    assert_eq!(binding["HostIp"], "127.0.0.1");
    let port = binding["HostPort"].as_str().unwrap().parse().unwrap();
    (container, port)
}

#[tokio::test]
#[ignore = "targeted pinned-Docker TLS probe; normal KIND CI verifies this factory too"]
async fn linux_postgres_certificate_has_a_real_sqlx_verified_handshake() {
    let directory = tempfile::tempdir().unwrap();
    let image = probe_image();
    let material = linux_material(directory.path(), &image);
    write_material(directory.path(), &material);
    let (container, port) = start_probe(directory.path(), &image);
    let deadline = Instant::now() + Duration::from_secs(60);
    while !Command::new("docker")
        .args([
            "exec",
            &container.0,
            "pg_isready",
            "-h",
            "127.0.0.1",
            "-U",
            "postgres",
            "-d",
            "smoke",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
    {
        assert!(
            Instant::now() < deadline,
            "isolated TLS PostgreSQL did not start"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let status = Command::new("docker").args(["exec", &container.0, "psql", "-X", "-v", "ON_ERROR_STOP=1", "-U", "postgres", "-d", "smoke", "-c",
        "CREATE ROLE navigator_tls_probe LOGIN NOSUPERUSER PASSWORD 'synthetic'; GRANT CONNECT ON DATABASE smoke TO navigator_tls_probe;"])
        .stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert!(status.success());
    let options = sqlx::postgres::PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .database("smoke")
        .username("navigator_tls_probe")
        .password("synthetic")
        .ssl_mode(sqlx::postgres::PgSslMode::VerifyFull)
        .ssl_root_cert(directory.path().join("ca.crt"));
    let mut connection = sqlx::PgConnection::connect_with(&options).await.unwrap();
    let row =
        sqlx::query("SELECT current_user AS role, ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid()")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(row.get::<String, _>("role"), "navigator_tls_probe");
    assert!(row.get::<bool, _>("ssl"));
    connection.close().await.unwrap();
    let wrong = server_certificate("navigator").unwrap();
    let wrong_path = directory.path().join("wrong-ca.crt");
    std::fs::write(&wrong_path, wrong.ca).unwrap();
    assert!(
        sqlx::PgConnection::connect_with(&options.clone().ssl_root_cert(wrong_path))
            .await
            .is_err()
    );
    sqlx::PgConnection::connect_with(&options)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let operator = store::postgres::PostgresDb::connect(options.username("postgres"))
        .await
        .unwrap();
    Box::pin(operator.migrate_embedded(None)).await.unwrap();
    assert_clause_catalog(&operator).await;
}

async fn assert_clause_catalog(pg: &store::postgres::PostgresDb) {
    let library = rules::clauses::library_clauses().unwrap();
    store::clauses::seed(pg, &library).await.unwrap();
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM clause")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let governing_law: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM clause WHERE code='governing_law')")
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert!(before > 0 && governing_law);
    store::clauses::seed(pg, &library).await.unwrap();
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM clause")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(before, after);
}

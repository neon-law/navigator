//! Git smart HTTP v0 over the workspace TLS client. libgit2 verifies the
//! downloaded object graph and builds the incremental pack entirely offline.
use super::SyncResult;
use anyhow::{ensure, Context, Result};
use git2::{ErrorCode, Oid, Repository};
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use std::io::{Read, Seek, Write};
use std::time::Duration;

pub(super) struct Access {
    pub(super) url: String,
    pub(super) user: String,
    pub(super) password: String,
}

fn packet(data: &[u8]) -> Vec<u8> {
    let mut bytes = format!("{:04x}", data.len() + 4).into_bytes();
    bytes.extend_from_slice(data);
    bytes
}

fn read_packet(input: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut header = [0; 4];
    input.read_exact(&mut header)?;
    let len = usize::from_str_radix(std::str::from_utf8(&header)?, 16)?;
    if len == 0 {
        return Ok(None);
    }
    ensure!((4..=65520).contains(&len), "invalid Git packet length");
    let mut payload = vec![0; len - 4];
    input.read_exact(&mut payload)?;
    Ok(Some(payload))
}

impl Access {
    fn response(&self, request: RequestBuilder, kind: &str) -> Result<Response> {
        let response = request
            .basic_auth(&self.user, Some(&self.password))
            .header(ACCEPT, kind)
            .send()?;
        ensure!(response.status() == 200, "Git HTTP request declined");
        ensure!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                == Some(kind),
            "unexpected Git content type"
        );
        Ok(response)
    }

    fn advertised_main(&self, http: &Client, service: &str) -> Result<Oid> {
        let url = format!("{}/info/refs", self.url);
        let mut res = self.response(
            http.get(url).query(&[("service", service)]),
            &format!("application/x-{service}-advertisement"),
        )?;
        ensure!(
            read_packet(&mut res)?.as_deref() == Some(format!("# service={service}\n").as_bytes())
                && read_packet(&mut res)?.is_none(),
            "unexpected Git advertisement"
        );
        let mut main = None;
        let mut report_status = false;
        while let Some(line) = read_packet(&mut res)? {
            let mut fields = line.splitn(2, |b| *b == 0);
            let reference = fields.next().unwrap_or_default().trim_ascii_end();
            if let Some(id) = reference.strip_suffix(b" refs/heads/main") {
                ensure!(main.is_none(), "duplicate main ref");
                ensure!(id.len() == 40, "unexpected main object format");
                main = Some(Oid::from_str(std::str::from_utf8(id)?)?);
            }
            if let Some(caps) = fields.next() {
                report_status = caps
                    .split(u8::is_ascii_whitespace)
                    .any(|c| c == b"report-status");
            }
        }
        ensure!(
            service != "git-receive-pack" || report_status,
            "missing push status capability"
        );
        main.context("main ref unavailable")
    }

    fn rpc(
        &self,
        http: &Client,
        service: &str,
        body: impl Into<reqwest::blocking::Body>,
    ) -> Result<Response> {
        self.response(
            http.post(format!("{}/{service}", self.url))
                .header(CONTENT_TYPE, format!("application/x-{service}-request"))
                .body(body),
            &format!("application/x-{service}-result"),
        )
    }
}

pub(super) fn synchronize(
    primary: &Access,
    mirror: &Access,
    ahead: Oid,
    behind: Oid,
) -> Result<SyncResult> {
    let http = Client::builder()
        .user_agent(concat!("Navigator/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(300))
        .build()?;
    if primary.advertised_main(&http, "git-upload-pack")? != ahead
        || mirror.advertised_main(&http, "git-receive-pack")? != behind
    {
        return Ok(SyncResult::ChangedDuringSync);
    }

    // With no haves, deltas against an external base or side-band requested,
    // upload-pack sends one NAK and a self-contained pack.
    let mut want = packet(format!("want {ahead}\n").as_bytes());
    want.extend_from_slice(b"00000009done\n");
    let mut fetched = primary.rpc(&http, "git-upload-pack", want)?;
    ensure!(
        read_packet(&mut fetched)?.as_deref() == Some(b"NAK\n"),
        "expected pack acknowledgement"
    );
    let dir = tempfile::tempdir()?;
    let repo = Repository::init_bare(dir.path())?;
    let odb = repo.odb()?;
    let mut indexer = odb.packwriter()?;
    std::io::copy(&mut fetched, &mut indexer)?;
    indexer.commit()?;
    repo.find_commit(ahead)?;
    match repo.find_commit(behind) {
        Err(e) if e.code() == ErrorCode::NotFound => return Ok(SyncResult::Diverged),
        other => {
            other?;
        }
    }
    if !repo.graph_descendant_of(ahead, behind)? {
        return Ok(SyncResult::Diverged);
    }

    let mut walk = repo.revwalk()?;
    walk.push(ahead)?;
    walk.hide(behind)?;
    let mut pack = repo.packbuilder()?;
    pack.insert_walk(&mut walk)?;
    let mut body = tempfile::tempfile()?;
    // receive-pack performs the old-object comparison under the ref lock.
    body.write_all(&packet(
        format!("{behind} {ahead} refs/heads/main\0report-status\n").as_bytes(),
    ))?;
    body.write_all(b"0000")?;
    pack.foreach(|chunk| body.write_all(chunk).is_ok())?;
    body.rewind()?;
    if primary.advertised_main(&http, "git-upload-pack")? != ahead {
        return Ok(SyncResult::ChangedDuringSync);
    }
    let mut result = mirror.rpc(&http, "git-receive-pack", body)?;
    ensure!(
        read_packet(&mut result)?.as_deref() == Some(b"unpack ok\n")
            && read_packet(&mut result)?.as_deref() == Some(b"ok refs/heads/main\n")
            && read_packet(&mut result)?.is_none(),
        "mirror declined fast-forward"
    );
    Ok(SyncResult::Advanced)
}

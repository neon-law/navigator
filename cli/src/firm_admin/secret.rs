//! Manage encrypted Firm credentials using the caller's saved site session.
use std::{
    io::{self, IsTerminal, Read},
    process::ExitCode,
};

use super::{HostOpt, Site};
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use model::stripe_keys::{Account, Directory, KeyKind, Mode};
use reqwest::Method;

#[derive(Subcommand)]
pub enum Command {
    /// Read the Stripe key from a pipe and rotate the selected credential.
    Set(Target),
    /// Show active Stripe key metadata for the Firm.
    List(Firm),
    /// Revoke the selected credential in Navigator.
    Delete(Target),
}

#[derive(Args)]
pub struct Firm {
    #[arg(long)]
    firm: String,
    #[command(flatten)]
    host: HostOpt,
}

#[derive(Args)]
pub struct Target {
    #[command(flatten)]
    firm: Firm,
    #[arg(long, value_parser = ["stripe"])]
    provider: String,
    #[arg(long)]
    account: Account,
    #[arg(long)]
    kind: KeyKind,
    #[arg(long)]
    mode: Mode,
}

fn read_key(input: impl Read, kind: KeyKind, mode: Mode) -> Result<String> {
    let mut value = String::new();
    input
        .take(4097)
        .read_to_string(&mut value)
        .context("read Stripe key from standard input")?;
    if value.len() > 4096 {
        bail!("Stripe key is too long");
    }
    value.truncate(value.trim_end_matches(['\r', '\n']).len());
    if !mode.accepts(kind, &value) {
        bail!("Stripe key prefix or format does not match the selected kind and mode");
    }
    Ok(value)
}

impl Command {
    async fn execute(&self) -> Result<()> {
        let firm = match self {
            Self::Set(t) | Self::Delete(t) => &t.firm,
            Self::List(f) => f,
        };
        let (base, token) = crate::remote::resolve(firm.host.host.as_deref())?;
        let site = Site {
            http: crate::remote::authenticated_client_builder(&base)?.build()?,
            base,
            token,
        };
        let id = site.firm(&firm.firm).await?.id;
        let path = format!("{}/app/api/firms/{id}/secrets", site.base);
        let response = site.http.get(&path).bearer_auth(&site.token).send().await?;
        if !response.status().is_success() {
            bail!("read Firm secret metadata refused ({})", response.status());
        }
        let directory: Directory = response.json().await.context("read Firm secret metadata")?;
        let target = match self {
            Self::List(_) => {
                for key in directory.keys {
                    let timestamp = chrono::DateTime::parse_from_rfc3339(&key.updated_at)?;
                    let mode = key
                        .mode
                        .map_or_else(|| "unrecorded".to_string(), |m| m.to_string());
                    println!(
                        "stripe\t{}\t{}\t{mode}\t{}",
                        key.account,
                        key.kind,
                        timestamp.to_rfc3339()
                    );
                }
                return Ok(());
            }
            Self::Set(t) | Self::Delete(t) => t,
        };
        if target.mode != directory.mode {
            bail!(
                "this deployment accepts only {} Stripe keys",
                directory.mode
            );
        }
        let path = format!(
            "{path}/{}/{}/{}/{}",
            target.provider, target.account, target.kind, target.mode
        );
        let request = if matches!(self, Self::Set(_)) {
            let stdin = io::stdin();
            if stdin.is_terminal() {
                bail!("pipe the Stripe key into standard input");
            }
            let value = read_key(stdin.lock(), target.kind, target.mode)?;
            site.http
                .put(path)
                .header("Content-Type", "application/octet-stream")
                .body(value)
        } else {
            site.http
                .request(Method::DELETE, path)
                .header("Content-Length", "0")
        };
        let status = request.bearer_auth(&site.token).send().await?.status();
        if status != reqwest::StatusCode::NO_CONTENT {
            bail!("Firm secret operation refused ({status})");
        }
        Ok(())
    }

    pub async fn run(&self) -> ExitCode {
        match self.execute().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("navigator: {error:#}");
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stdin_accepts_a_clipboard_newline_and_refusals_omit_the_input() {
        assert_eq!(
            read_key(&b"sk_test_example\r\n"[..], KeyKind::Secret, Mode::Sandbox).unwrap(),
            "sk_test_example"
        );
        let value = "sk_live_example";
        let error = read_key(value.as_bytes(), KeyKind::Secret, Mode::Sandbox).unwrap_err();
        assert!(!error.to_string().contains(value));
    }
}

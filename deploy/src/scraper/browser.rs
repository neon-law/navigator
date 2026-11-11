//! The live-Chromium half of the scraper session layer: everything here
//! spawns the pinned chromedriver, connects a `WebDriver` client, or drives
//! cookies into a running browser. None of it can execute in the coverage
//! job (no browser harness — the same class as `features/src/webdriver.rs`,
//! proven by the deploy.yml browser/e2e gates instead), which is why the
//! file is excluded from that job's coverage denominator. The pure
//! decisions — session config and its redacted `Debug`, cookie and state
//! serialization, the driver command, the capability shape, port polling —
//! stay in [`super::session`], counted, with their tests.

use fantoccini::ClientBuilder;

use crate::scraper::adapter::SiteKey;
use crate::scraper::session::{
    load_state, BrowserSession, SessionConfig, SessionError, SessionFactory,
};

/// The production session factory: every `open` spawns the pinned
/// chromedriver, connects a client, and restores the site's cookies.
#[derive(Debug)]
pub(crate) struct ChromiumSessions {
    config: SessionConfig,
}

impl ChromiumSessions {
    #[must_use]
    pub(crate) fn new(config: SessionConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl SessionFactory for ChromiumSessions {
    async fn open(&self, site: &SiteKey) -> Result<BrowserSession, SessionError> {
        BrowserSession::connect(&self.config, site).await
    }
}

impl BrowserSession {
    /// Spawn the pinned chromedriver, connect to it, and restore the site's
    /// persisted cookies.
    ///
    /// # Errors
    ///
    /// Driver launch, port timeout, connection, or cookie restore failures
    /// all abort before any adapter runs; the spawned driver process is
    /// killed on every error path.
    pub(crate) async fn connect(
        config: &SessionConfig,
        site: &SiteKey,
    ) -> Result<Self, SessionError> {
        let mut driver = super::session::chromedriver_command(
            &config.chrome.chromedriver,
            config.webdriver_port,
        )
        .spawn()
        .map_err(|source| SessionError::DriverLaunch {
            path: config.chrome.chromedriver.clone(),
            source,
        })?;
        if let Err(error) = super::session::poll_port_until(
            config.webdriver_port,
            super::session::DRIVER_START_TIMEOUT,
        )
        .await
        {
            let _ = driver.kill();
            return Err(error);
        }

        let chrome_binary = config.chrome.chrome.to_str().ok_or_else(|| {
            let _ = driver.kill();
            SessionError::NonUtf8Path {
                path: config.chrome.chrome.clone(),
            }
        })?;
        let caps = super::session::webdriver_capabilities(Some(chrome_binary), config.headless);
        let url = format!("http://127.0.0.1:{}", config.webdriver_port);
        // rustls, not native-tls: the static musl `navigator` cannot link
        // OpenSSL. Pin the process crypto provider first, since the workspace
        // compiles both ring and aws-lc-rs and rustls will not pick one.
        store::surreal::install_tls_provider();
        let mut builder = match ClientBuilder::rustls() {
            Ok(builder) => builder,
            Err(error) => {
                let _ = driver.kill();
                return Err(SessionError::Tls(error));
            }
        };
        let client = match builder.capabilities(caps).connect(&url).await {
            Ok(client) => client,
            Err(error) => {
                let _ = driver.kill();
                return Err(SessionError::Connect(error));
            }
        };

        let session = Self::assemble(
            client,
            driver,
            super::session::session_state_path(&config.state_dir, site),
        );
        session.import_cookies().await?;
        Ok(session)
    }

    /// Restore the site's persisted cookies into the fresh session. An empty
    /// or absent state file is the first-run shape and restores nothing.
    async fn import_cookies(&self) -> Result<(), SessionError> {
        let Some(client) = self.client_ref() else {
            return Ok(());
        };
        for persisted in &load_state(self.state_path())?.cookies {
            let cookie = super::session::restorable_cookie(persisted);
            client
                .add_cookie(cookie)
                .await
                .map_err(SessionError::Cookie)?;
        }
        Ok(())
    }
}

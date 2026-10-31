//! Chromium-driven portal sessions, built on the pins the browser e2e
//! harness already uses: the Chrome for Testing resolver
//! (`crate::devx::chrome`) picks the browser, a spawned matching
//! chromedriver serves WebDriver, and a fantoccini client drives it.
//!
//! The session owns two things for its site: the driver process (killed on
//! teardown, however the run ends) and the cookie file under the site's
//! `.navigator/scraper` directory, so a portal that issued a session cookie
//! on Monday does not make the operator's firm re-login on Tuesday. Restored
//! cookies only shorten login — adapters still run their login flow, which
//! becomes a no-op when the portal already remembers the session.

use std::path::{Path, PathBuf};

use fantoccini::cookies::Cookie;
use fantoccini::{Client, ClientBuilder};
use serde::{Deserialize, Serialize};

use crate::devx::chrome::ChromeForTesting;
use crate::scraper::adapter::SiteKey;

/// How long `connect` waits for a freshly spawned chromedriver to open its
/// port before giving up. Driver startup is sub-second in practice; the
/// budget absorbs a cold, loaded CI runner.
const DRIVER_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Why a session could not be opened, persisted, or torn down.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionError {
    #[error("the chromedriver binary at {path} could not be launched")]
    DriverLaunch {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("chromedriver did not open port {port} in time")]
    DriverTimeout { port: u16 },
    #[error("the WebDriver connection failed")]
    Connect(#[from] fantoccini::error::NewSessionError),
    #[error("the WebDriver operation failed")]
    WebDriver(#[from] fantoccini::error::CmdError),
    #[error("the session cookies could not be read or saved")]
    Cookie(fantoccini::error::CmdError),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the session state file is not valid")]
    Serialize(#[from] serde_json::Error),
    #[error("the pinned Chrome binary path is not valid UTF-8: {path:?}")]
    NonUtf8Path { path: PathBuf },
}

/// One cookie as it persists between runs: the WebDriver subset that
/// restores a session, with `expiry` kept for fidelity but never restored
/// (an expired cookie is worthless to re-import).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistedCookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    pub secure: Option<bool>,
    pub expiry: Option<u64>,
}

/// What a site's session state file holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionState {
    pub cookies: Vec<PersistedCookie>,
}

/// The resolved browser pin, driver port, and state directory one site's
/// sessions run with.
pub(crate) struct SessionConfig {
    pub chrome: ChromeForTesting,
    pub webdriver_port: u16,
    pub headless: bool,
    /// Directory the per-site cookie files live under
    /// (`<state_dir>/<site>/session.json`); a Project checkout points this
    /// at its git-ignored `.navigator/scraper` directory.
    pub state_dir: PathBuf,
}

impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionConfig")
            .field("webdriver_port", &self.webdriver_port)
            .field("headless", &self.headless)
            .field("state_dir", &self.state_dir)
            .finish_non_exhaustive()
    }
}

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

/// Opens the portal session for one site. Production runs use
/// [`ChromiumSessions`]; orchestration tests inject a stub factory so no
/// test ever launches a browser.
#[async_trait::async_trait]
pub(crate) trait SessionFactory: Send + Sync {
    async fn open(&self, site: &SiteKey) -> Result<BrowserSession, SessionError>;
}

/// One live portal session: the WebDriver client, the chromedriver process
/// backing it, and the cookie file the session restores from and persists to.
pub(crate) struct BrowserSession {
    client: Option<Client>,
    driver: Option<std::process::Child>,
    state_path: PathBuf,
}

impl BrowserSession {
    /// A session with no live WebDriver behind it — the stub session
    /// factory hands these out so orchestration tests never launch a
    /// browser. Cookie persistence and shutdown degrade to no-ops.
    #[cfg(test)]
    pub(crate) fn offline(site: &SiteKey, state_path: PathBuf) -> Self {
        let _ = site;
        Self {
            client: None,
            driver: None,
            state_path,
        }
    }

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
        let mut driver = chromedriver_command(
            &config.chrome.chromedriver,
            config.webdriver_port,
        )
        .spawn()
        .map_err(|source| SessionError::DriverLaunch {
            path: config.chrome.chromedriver.clone(),
            source,
        })?;
        if let Err(error) = poll_port_until(config.webdriver_port, DRIVER_START_TIMEOUT).await {
            let _ = driver.kill();
            return Err(error);
        }

        let chrome_binary = config.chrome.chrome.to_str().ok_or_else(|| {
            let _ = driver.kill();
            SessionError::NonUtf8Path {
                path: config.chrome.chrome.clone(),
            }
        })?;
        let caps = webdriver_capabilities(Some(chrome_binary), config.headless);
        let url = format!("http://127.0.0.1:{}", config.webdriver_port);
        let client = match ClientBuilder::native().capabilities(caps).connect(&url).await {
            Ok(client) => client,
            Err(error) => {
                let _ = driver.kill();
                return Err(SessionError::Connect(error));
            }
        };

        let session = Self {
            client: Some(client),
            driver: Some(driver),
            state_path: session_state_path(&config.state_dir, site),
        };
        session.import_cookies().await?;
        Ok(session)
    }

    /// Write the session's current cookies to its state file. A session
    /// opened without a connection (the orchestration-test stub) has nothing
    /// to persist and succeeds without writing.
    ///
    /// # Errors
    ///
    /// When the WebDriver read or the state-file write fails.
    pub(crate) async fn persist_cookies(&self) -> Result<(), SessionError> {
        let Some(client) = self.client.as_ref() else {
            return Ok(());
        };
        let cookies = client.get_all_cookies().await.map_err(SessionError::Cookie)?;
        let state = SessionState {
            cookies: cookies.iter().map(persisted_cookie).collect(),
        };
        save_state(&self.state_path, &state)
    }

    /// Persist cookies, close the WebDriver session, and stop the driver
    /// process. Runs on every orchestration path, success or failure.
    ///
    /// # Errors
    ///
    /// When persisting or closing fails; the driver process is stopped
    /// regardless.
    pub(crate) async fn shutdown(mut self) -> Result<(), SessionError> {
        self.persist_cookies().await?;
        if let Some(client) = self.client.take() {
            client.close().await.map_err(SessionError::WebDriver)?;
        }
        if let Some(child) = self.driver.as_mut() {
            // chromedriver exits when its session closes; the kill is the
            // backstop for the paths where it did not.
            let _ = child.kill();
        }
        Ok(())
    }

    /// Restore the site's persisted cookies into the fresh session. An empty
    /// or absent state file is the first-run shape and restores nothing.
    async fn import_cookies(&self) -> Result<(), SessionError> {
        let Some(client) = self.client.as_ref() else {
            return Ok(());
        };
        for persisted in &load_state(&self.state_path)?.cookies {
            let mut cookie = Cookie::new(persisted.name.clone(), persisted.value.clone());
            if let Some(path) = &persisted.path {
                cookie.set_path(path.clone());
            }
            if let Some(domain) = &persisted.domain {
                cookie.set_domain(domain.clone());
            }
            if let Some(secure) = persisted.secure {
                cookie.set_secure(secure);
            }
            client
                .add_cookie(cookie)
                .await
                .map_err(SessionError::Cookie)?;
        }
        Ok(())
    }

    /// A disconnected session for orchestration tests: `persist_cookies` and
    /// `shutdown` succeed without touching a browser, and no driver process
    /// exists to leak.
    #[cfg(test)]
    pub(crate) fn stub() -> Self {
        Self {
            client: None,
            driver: None,
            state_path: std::env::temp_dir().join(format!(
                "navigator-scraper-stub-{}",
                uuid::Uuid::now_v7()
            )),
        }
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        // `shutdown` is the real teardown and runs on every orchestration
        // path; this backstop only makes sure a session dropped some other
        // way never leaks the driver process.
        if let Some(child) = self.driver.as_mut() {
            let _ = child.kill();
        }
    }
}

/// A WebDriver cookie as the fantoccini client reports it, reduced to the
/// persisted subset.
fn persisted_cookie(cookie: &Cookie<'_>) -> PersistedCookie {
    PersistedCookie {
        name: cookie.name().to_owned(),
        value: cookie.value().to_owned(),
        domain: cookie.domain().map(str::to_owned),
        path: cookie.path().map(str::to_owned),
        secure: cookie.secure(),
        expiry: cookie
            .expires()
            .and_then(|expires| expires.datetime())
            .and_then(|at| u64::try_from(at.unix_timestamp()).ok()),
    }
}

/// Where one site's cookie file lives under the state directory.
#[must_use]
pub(crate) fn session_state_path(state_dir: &Path, site: &SiteKey) -> PathBuf {
    state_dir.join(site.as_str()).join("session.json")
}

/// Persist `state` atomically: write beside the target, then rename into
/// place, so an interrupted write never leaves a half-read cookie file.
fn save_state(path: &Path, state: &SessionState) -> Result<(), SessionError> {
    let bytes = serde_json::to_vec_pretty(state)?;
    crate::scraper::fs::atomic_write(path, &bytes).map_err(|source| SessionError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Read a site's persisted session state. An absent file is the first-run
/// shape and reads as empty, not as an error.
fn load_state(path: &Path) -> Result<SessionState, SessionError> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(SessionError::Serialize),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(SessionState::default())
        }
        Err(source) => Err(SessionError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The chromedriver launch command: the pinned driver binary listening on
/// its port. Pure so the exact invocation is pinned by test.
fn chromedriver_command(driver: &Path, port: u16) -> std::process::Command {
    let mut command = std::process::Command::new(driver);
    command.arg(format!("--port={port}"));
    command
}

/// The WebDriver capabilities for a pinned Chrome: the binary to drive, the
/// sandbox flags the e2e harness uses, and headless mode unless the operator
/// asked to watch. Pure so the capability shape is pinned by test.
fn webdriver_capabilities(
    chrome_binary: Option<&str>,
    headless: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let mut args: Vec<&str> = vec![
        "--no-sandbox",
        "--disable-dev-shm-usage",
        "--window-size=1280,800",
    ];
    if headless {
        args.push("--headless=new");
    }
    let mut options = serde_json::Map::new();
    if let Some(binary) = chrome_binary {
        options.insert(
            "binary".to_owned(),
            serde_json::Value::String(binary.to_owned()),
        );
    }
    options.insert(
        "args".to_owned(),
        serde_json::Value::Array(
            args.iter()
                .map(|argument| serde_json::Value::String((*argument).to_owned()))
                .collect(),
        ),
    );
    let mut caps = serde_json::Map::new();
    caps.insert(
        "goog:chromeOptions".to_owned(),
        serde_json::Value::Object(options),
    );
    caps
}

/// Wait until something is accepting TCP connections on `port`, or the
/// deadline passes. Loopback only — it is how `connect` knows the freshly
/// spawned chromedriver is ready.
async fn poll_port_until(port: u16, deadline: std::time::Duration) -> Result<(), SessionError> {
    let started = std::time::Instant::now();
    loop {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return Ok(());
        }
        if started.elapsed() >= deadline {
            return Err(SessionError::DriverTimeout { port });
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PersistedCookie, SessionState, chromedriver_command, load_state, poll_port_until,
        save_state, session_state_path, webdriver_capabilities,
    };
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("navigator-scraper-{name}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn sample_cookie(name: &str, expiry: Option<u64>) -> PersistedCookie {
        PersistedCookie {
            name: name.to_owned(),
            value: "v".to_owned(),
            domain: Some("portal.example.com".to_owned()),
            path: Some("/".to_owned()),
            secure: Some(true),
            expiry,
        }
    }

    #[test]
    fn session_state_round_trips_through_disk() {
        let dir = temp_dir("state-round-trip");
        let path = dir.join("session.json");
        let state = SessionState {
            cookies: vec![sample_cookie("session", Some(1_800_000_000)), sample_cookie("pref", None)],
        };
        save_state(&path, &state).expect("save state");
        assert_eq!(load_state(&path).expect("load state"), state);
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_missing_state_file_reads_as_empty() {
        let dir = temp_dir("state-missing");
        let state = load_state(&dir.join("session.json")).expect("load missing state");
        assert!(state.cookies.is_empty());
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn saving_leaves_no_temp_files_behind() {
        let dir = temp_dir("state-atomic");
        let path = dir.join("nested").join("session.json");
        save_state(&path, &SessionState::default()).expect("save once");
        save_state(&path, &SessionState::default()).expect("save twice");
        let entries: Vec<_> = walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["session.json".to_owned()]);
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn session_paths_are_per_site() {
        let site = crate::scraper::adapter::SiteKey::new("la-superior").expect("valid key");
        assert_eq!(
            session_state_path(Path::new("/state"), &site),
            PathBuf::from("/state/la-superior/session.json")
        );
    }

    #[test]
    fn capabilities_pin_the_binary_and_headless_mode() {
        let headless = webdriver_capabilities(Some("/chrome/chrome"), true);
        let options = headless
            .get("goog:chromeOptions")
            .and_then(|value| value.as_object())
            .expect("chrome options object");
        assert_eq!(
            options.get("binary").and_then(serde_json::Value::as_str),
            Some("/chrome/chrome")
        );
        let args = options
            .get("args")
            .and_then(serde_json::Value::as_array)
            .expect("args array");
        assert!(args.iter().any(|value| value.as_str() == Some("--headless=new")));

        let headed = webdriver_capabilities(None, false);
        let options = headed
            .get("goog:chromeOptions")
            .and_then(|value| value.as_object())
            .expect("chrome options object");
        assert!(!options.contains_key("binary"));
        let args = options
            .get("args")
            .and_then(serde_json::Value::as_array)
            .expect("args array");
        assert!(!args.iter().any(|value| value.as_str() == Some("--headless=new")));
    }

    #[test]
    fn the_driver_command_names_its_port() {
        let command = chromedriver_command(Path::new("/drivers/chromedriver"), 9515);
        assert_eq!(command.get_program(), Path::new("/drivers/chromedriver"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![std::ffi::OsStr::new("--port=9515")]
        );
    }

    #[tokio::test]
    async fn port_polling_finds_a_listening_loopback_port() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local address").port();
        poll_port_until(port, Duration::from_secs(2))
            .await
            .expect("the bound listener is found");
    }

    #[tokio::test]
    async fn port_polling_times_out_when_nothing_listens() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local address").port();
        drop(listener);
        let result = poll_port_until(port, Duration::from_millis(300)).await;
        assert!(result.is_err(), "a closed port must time out");
    }
}

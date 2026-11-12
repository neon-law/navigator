/// Whether a matter code is safe as a URL segment, a bare-repo directory name,
/// and an object-storage key prefix.
///
/// Lowercase letters, digits, and single hyphens; alphanumeric at both ends,
/// and not a segment Navigator routes on its own.
///
/// `cloud::workspace` holds the single definition of that shape and this calls
/// it, so a Project code and its repository's directory name cannot drift
/// apart. The rationale for each shape restriction lives there. The
/// `repository_url` *column*, in contrast, is not composed from the code — it
/// is a whole URL stored on the row, validated by [`is_valid_repository_url`].
///
/// The reserved-code refusal is the second half. A code is a route segment —
/// `/app/projects/{code}/portal` — and `/app/projects/new` is the matter-open
/// form, so `new` is well-formed and still refused. Which side of a genuine
/// collision would win depends on Axum registration order, so the code is
/// refused rather than the precedence reasoned about. The engine carries the
/// same refusal as an `ASSERT` on `project.code`, because this function only
/// guards the write paths that call it.
#[must_use]
pub fn is_valid_code(code: &str) -> bool {
    crate::workspace::is_valid_slug(code)
        && !crate::workspace::RESERVED_PROJECT_CODES.contains(&code)
}

/// Human-readable reason [`is_valid_resource_url`] refuses a value, used as
/// the caller-correctable message on the command boundary.
pub const RESOURCE_URL_INVALID: &str =
    "A resource link must be an http(s):// URL naming a host and a path.";

/// Whether a value is usable as one of the matter's collaboration resource
/// links — the two Slack channels and the two Notion pages.
///
/// Every one of these is rendered as an `href` on the matter page, to lawyer
/// and client alike, which is the whole reason they are validated rather than
/// merely trimmed. The rule is [`is_valid_repository_url`]'s: `http(s)` only,
/// a real host, a non-empty path, no whitespace, and no embedded credential.
/// A `javascript:` or `data:` value would execute instead of navigating, and a
/// `user:token@host` value would put a secret into a rendered page.
///
/// It is deliberately the *same* rule rather than a looser one. These columns
/// hold third-party addresses the firm pastes in, so there is no shape to
/// check beyond "this is a link that navigates somewhere" — and a resource
/// panel that trusted one column more than another would be a gap waiting for
/// the next field to be added to the wrong half.
#[must_use]
pub fn is_valid_resource_url(url: &str) -> bool {
    is_valid_repository_url(url)
}

/// Human-readable reason [`is_valid_calendar_id`] refuses a value.
pub const CALENDAR_ID_INVALID: &str =
    "A calendar id is the Calendar ID shown in the Google Calendar's settings, not a link.";

/// Human-readable reason [`is_valid_drive_folder_id`] refuses a value.
pub const DRIVE_FOLDER_ID_INVALID: &str =
    "A Drive folder id is the opaque id of the folder in the Firm's Projects shared drive, not a link.";

/// Human-readable reason [`drive_folder_id_from_input`] refuses what a lawyer
/// entered on the CLI or the matter edit form, where a link is welcome.
pub const DRIVE_FOLDER_INPUT_INVALID: &str =
    "A Drive folder is its drive.google.com folder link or its id, a folder in the Firm's Projects shared drive.";

/// The Drive folder id a lawyer typed or pasted: a bare id, or a
/// `drive.google.com` folder link in any of the shapes Drive hands out —
/// `/drive/folders/<id>`, `/drive/u/<n>/folders/<id>`, `/open?id=<id>`, and
/// `/folderview?id=<id>`, with or without a query string. A shared-drive
/// folder has the same link shapes as any other folder, so it needs no case
/// of its own. `None` for anything else, including a link to another host.
///
/// Refused input is reported with [`DRIVE_FOLDER_INPUT_INVALID`]. The result
/// always satisfies [`is_valid_drive_folder_id`], so the CLI and
/// the matter edit form store the same id whichever form was entered.
#[must_use]
pub fn drive_folder_id_from_input(value: &str) -> Option<String> {
    let value = value.trim();
    if is_valid_drive_folder_id(value) {
        return Some(value.to_string());
    }
    let url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str() != Some("drive.google.com") {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let id = match segments.as_slice() {
        ["drive", "folders", id] | ["drive", "u", _, "folders", id] => (*id).to_string(),
        ["open" | "folderview"] => url
            .query_pairs()
            .find(|(key, _)| key == "id")
            .map(|(_, id)| id.into_owned())?,
        _ => return None,
    };
    is_valid_drive_folder_id(&id).then_some(id)
}

/// Whether a value has the shape of a Google Drive file id: one token of at
/// most 128 characters, letters, digits, `-`, and `_` only.
///
/// The id is placed in a Drive API path, so anything that could change the
/// path or the query is refused rather than escaped. Whether the folder
/// exists — and whether it is the Project's ingest folder — is the provider's
/// answer (`DriveService::is_ingest_folder`), not this one.
#[must_use]
pub fn is_valid_drive_folder_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Whether a value has the shape of a Google Calendar id: one token of at most
/// 255 characters, letters, digits, and `.`, `_`, `-`, `+`, `@` only.
///
/// The id is placed in a Calendar API path and in a `calendar.google.com`
/// link, so anything that could change the path or the query is refused rather
/// than escaped. Whether the calendar exists is the provider's answer, not
/// this one.
#[must_use]
pub fn is_valid_calendar_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '@'))
}

/// The closed vocabulary of reasons for a direct matter close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosureReason {
    PitchDeclined,
    PitchLapsed,
    PitchWithdrawn,
    PitchSuperseded,
    EngagementCompleted,
    ClientTerminated,
    FirmWithdrew,
}

impl ClosureReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PitchDeclined => "pitch_declined",
            Self::PitchLapsed => "pitch_lapsed",
            Self::PitchWithdrawn => "pitch_withdrawn",
            Self::PitchSuperseded => "pitch_superseded",
            Self::EngagementCompleted => "engagement_completed",
            Self::ClientTerminated => "client_terminated",
            Self::FirmWithdrew => "firm_withdrew",
        }
    }

    #[must_use]
    pub const fn is_pitch(self) -> bool {
        matches!(
            self,
            Self::PitchDeclined | Self::PitchLapsed | Self::PitchWithdrawn | Self::PitchSuperseded
        )
    }

    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::EngagementCompleted | Self::ClientTerminated | Self::FirmWithdrew
        )
    }
}

impl std::str::FromStr for ClosureReason {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pitch_declined" => Ok(Self::PitchDeclined),
            "pitch_lapsed" => Ok(Self::PitchLapsed),
            "pitch_withdrawn" => Ok(Self::PitchWithdrawn),
            "pitch_superseded" => Ok(Self::PitchSuperseded),
            "engagement_completed" => Ok(Self::EngagementCompleted),
            "client_terminated" => Ok(Self::ClientTerminated),
            "firm_withdrew" => Ok(Self::FirmWithdrew),
            _ => Err(format!("unknown closure reason `{value}`")),
        }
    }
}

/// The reason an attorney reopened a closed Project.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReopenReason {
    NewMatterEvent,
}

/// Whether a value is usable as a Project's source repository URL.
///
/// Deliberately permissive about *where*: any forge, any organization, any
/// self-hosted host. It is strict about *what*, because this URL is both shown
/// to a lawyer as a link and handed to `git clone`:
///
/// - **`http://` or `https://` only.** A `file://`, `ssh://`, or `javascript:`
///   value would either read the serving host's own disk or render as a live
///   link, and neither is a repository the Firm meant to record.
/// - **A non-empty host and path.** `https://github.com` names a forge, not a
///   repository, so cloning it could never succeed.
/// - **No whitespace and no credentials.** A `user:token@host` URL would put a
///   secret in a column that is rendered into a page and logged.
#[must_use]
pub fn is_valid_repository_url(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return false;
    }
    let Some(("http" | "https", rest)) = trimmed.split_once("://") else {
        return false;
    };
    let Some((authority, path)) = rest.split_once('/') else {
        return false;
    };
    // `@` in the authority is an embedded credential; a bare host has none.
    !authority.is_empty() && !authority.contains('@') && !path.trim_matches('/').is_empty()
}

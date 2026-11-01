//! The forge seam behind `navigator project repository push`.
//!
//! The Project fleet is moving from GitHub to Forgejo, and the landing lane —
//! open the review request, arm merge-when-green, watch the required check,
//! capture the merge commit, then watch the deployment run for the merge
//! commit — must read identically to the agent on either forge. The verb
//! speaks to [`Forge`] and never to a forge's API; each forge's quirks live in
//! its own backend. GitHub reports a check as `<caller job> / <called job>`
//! and answers a merge-when-green arm with "clean status" exactly when the
//! request is already mergeable; both details are the GitHub backend's
//! problem, never the verb's or a synced skill's.

use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;

/// A forge run pinned to the commit whose effects it verifies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DeploymentRun {
    pub(crate) url: String,
    pub(crate) commit: String,
    pub(crate) conclusion: Option<String>,
    pub(crate) jobs: Vec<DeploymentJob>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DeploymentJob {
    pub(crate) name: String,
    pub(crate) conclusion: Option<String>,
}

/// A review request's coordinates once it exists on the forge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewRef {
    pub(crate) number: u64,
    /// The head commit the review request was opened from.
    pub(crate) head_sha: String,
    pub(crate) url: String,
}

/// What a backend needs to open a review request. Always ready for review —
/// a draft is how a ready request is held open, and this lane holds nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewRequest {
    pub(crate) head_branch: String,
    pub(crate) base_branch: String,
    pub(crate) title: String,
    pub(crate) body: String,
}

/// What arming merge-when-green did. GitHub's arm is read back; Forgejo
/// acknowledges scheduling and the watcher requires an observed merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArmOutcome {
    /// Merge-when-green is armed and was read back as enabled.
    Armed,
    /// An earlier run of the verb armed it already.
    AlreadyArmed,
    /// Forgejo acknowledged scheduling; its API exposes no queue read-back.
    /// The actual merged state is still required before reporting success.
    Scheduled,
    /// The forge would not queue because the request merges now, so the
    /// backend merged it directly, pinned to the commit it checked.
    MergedNow { merge_commit: String },
}

/// The required check's verdict on a review request's head commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheckVerdict {
    /// At least one required check is still running.
    Pending,
    /// Every required check passed.
    Green,
    /// A required check failed; the payload names it as the forge reports it.
    Failed(String),
    /// The review request cannot merge until its base's own changes are in.
    BranchBehind,
    /// The review request cannot merge until a required review resolves.
    HeldReview,
    /// The request's head and base have diverged enough that the forge
    /// refuses to compute a merge.
    Conflict,
}

/// The forge seam: everything the landing lane needs from a forge, named in
/// forge-neutral terms so the agent-facing verb and its report do not change
/// when the forge does.
#[async_trait]
pub(crate) trait Forge: Send + Sync {
    /// Who this run authenticates as; an error when no forge authentication
    /// is present (LAW-220 preflight, first question).
    async fn identity(&self) -> Result<String>;

    /// Whether the checkout's push remote answers; an error when it does not.
    async fn remote_reachable(&self) -> Result<()>;

    /// Open the review request, or return the one already open for the head
    /// branch: re-running the verb after a named stall must never open a
    /// second request for the same change.
    async fn open_review(&self, request: ReviewRequest) -> Result<ReviewRef>;

    /// Arm merge-when-green and read the arm back.
    async fn arm_merge(&self, review: &ReviewRef) -> Result<ArmOutcome>;

    /// The required check's verdict on the review request's head commit.
    async fn check_verdict(&self, review: &ReviewRef) -> Result<CheckVerdict>;

    /// The merge commit's sha once the request has merged, `None` while it
    /// is still open.
    async fn merge_commit(&self, review: &ReviewRef) -> Result<Option<String>>;

    /// The latest post-merge push run for this exact commit, including its jobs.
    async fn deployment(&self, merge_commit: &str) -> Result<Option<DeploymentRun>>;

    /// The pull-request CI run for the reviewed head, when the forge exposes it.
    async fn verification(&self, review: &ReviewRef) -> Result<Option<DeploymentRun>>;
}

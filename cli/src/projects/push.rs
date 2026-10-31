//! `navigator project repository push` — push this change to the navigator.
//!
//! One verb, run from a gated Project checkout, that lands the change and
//! follows it through the whole merge loop instead of an agent transcribing
//! `gh` per session (LAW-220): preflight the forge auth, the push remote,
//! commit signing, and this CLI release against the manifest's pin; branch,
//! signed commit, push; open the review request ready for review; arm
//! merge-when-green and read the arm back; watch the required check to a
//! verdict, naming each stall separately; capture the merge commit; then
//! hand the merge commit to the merge watcher. Every forge question goes
//! through the [`Forge`] seam, never through a forge's API.

use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chrono::Utc;

use super::forge::{ArmOutcome, CheckVerdict, Forge, ReviewRef, ReviewRequest};
use super::forge_github::GitHubForge;
use super::lifecycle::{git, git_text};
use super::manifest;

/// How often the lane re-reads the required check while it runs.
pub(crate) const CHECK_POLL: Duration = Duration::from_secs(15);
/// How long the lane waits for the required check and the merge to happen.
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// What the lane did, as the structured report renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaneReport {
    pub(crate) review_url: String,
    pub(crate) merge_commit: String,
}

/// Build the real backend and run the lane. The dispatch half is the only
/// untested seam here: it resolves live credentials, which no test can hold.
pub(crate) async fn dispatch(
    dir: &Path,
    base: &str,
    branch: Option<&str>,
    title: &str,
    body: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let forge = GitHubForge::from_checkout(dir)?;
    run(
        dir,
        base,
        branch,
        title,
        body,
        dry_run,
        &forge,
        tokio::time::sleep,
    )
    .await?;
    Ok(())
}

/// The landing lane. The `sleep` injection is what keeps the watch loop
/// testable: production passes `tokio::time::sleep`, tests pass an instant
/// no-op and drive the loop with a mocked forge.
pub(crate) async fn run<S, Fut>(
    dir: &Path,
    base: &str,
    branch: Option<&str>,
    title: &str,
    body: Option<&str>,
    dry_run: bool,
    forge: &dyn Forge,
    sleep: S,
) -> Result<LaneReport>
where
    S: Fn(Duration) -> Fut,
    Fut: Future<Output = ()>,
{
    println!("preflight:");
    let identity = forge
        .identity()
        .await
        .context("forge authentication is missing; authenticate and re-run")?;
    println!("  authenticated as {identity}");
    forge
        .remote_reachable()
        .await
        .context("the push remote did not answer")?;
    println!("  the push remote answers");
    check_signing(dir)?;
    println!("  commit signing is on");
    let pin = require_manifest(dir)
        .and_then(|pinned| check_version_pin(pinned.as_deref(), env!("CARGO_PKG_VERSION")))?;
    println!("  {pin}");

    let target = branch
        .map(str::to_string)
        .unwrap_or_else(|| branch_slug(title, Utc::now().date_naive()));
    if dry_run {
        println!(
            "dry run: would branch {target}, commit the change signed, push it to origin, open \
             a review request against {base} ready for review, arm merge-when-green and read \
             the arm back, watch the required check to a verdict, and follow the merge commit \
             through the deployment run."
        );
        bail!("dry run: nothing was pushed");
    }

    // Branch (LAW-220 step 2). An existing branch is checked out, not
    // clobbered: re-running the verb after a named stall continues the same
    // change.
    let current = git_text(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if current != target {
        let exists = git(
            dir,
            &["rev-parse", "--verify", &format!("refs/heads/{target}")],
        )?
        .status
        .success();
        if exists {
            git_text(dir, &["checkout", &target])
                .with_context(|| format!("check out the existing branch {target}"))?;
        } else {
            git_text(dir, &["checkout", "-b", &target])
                .with_context(|| format!("create the branch {target}"))?;
        }
        println!("branch: {target}");
    } else {
        println!("branch: already on {target}");
    }

    git_text(dir, &["add", "-A"]).context("stage the change")?;
    if git_text(dir, &["status", "--porcelain"])?.is_empty() {
        println!("commit: nothing to commit; continuing to the open review request");
    } else {
        git_text(dir, &["commit", "-S", "-m", title]).context("signed commit")?;
        println!("commit: signed");
    }

    let pushed = git(dir, &["push", "-u", "origin", &target])?;
    if !pushed.status.success() {
        bail!(
            "git push failed: {}",
            String::from_utf8_lossy(&pushed.stderr).trim()
        );
    }
    println!("push: {target} on origin");

    // Review request, ready for review — never a draft (LAW-220 step 2).
    let review = forge
        .open_review(ReviewRequest {
            head_branch: target.clone(),
            base_branch: base.to_string(),
            title: title.to_string(),
            body: body.unwrap_or_default().to_string(),
        })
        .await?;
    println!("review request: {} ({})", review.url, review.head_sha);

    // Merge-when-green, armed and read back (LAW-220 step 3), then the
    // required check to a verdict and the merge commit captured (steps 4–5).
    let outcome = forge.arm_merge(&review).await?;
    let merge_commit = match outcome {
        ArmOutcome::MergedNow { merge_commit } => {
            println!("merge: landed now, pinned to the checked commit");
            merge_commit
        }
        ArmOutcome::Armed | ArmOutcome::AlreadyArmed => watch_check(forge, &review, sleep).await?,
    };

    let report = LaneReport {
        review_url: review.url.clone(),
        merge_commit,
    };
    println!("{}", render_report(&report));
    Ok(report)
}

/// Read the manifest's CLI version pin. The verb runs from a Project
/// repository, so the manifest is not optional; one without a pin is
/// reported, not refused.
fn require_manifest(dir: &Path) -> Result<Option<String>> {
    let pinned = manifest::read(dir).unwrap_or_default().version;
    if pinned.is_none() {
        println!("  the manifest carries no CLI version pin");
    }
    Ok(pinned)
}

/// Commit signing is a preflight question, not a hope: the lane's commits
/// are signed because the gate requires it.
pub(crate) fn check_signing(dir: &Path) -> Result<()> {
    let setting = git(dir, &["config", "--get", "commit.gpgsign"])?;
    let value = String::from_utf8_lossy(&setting.stdout).trim().to_string();
    if setting.status.success() && value.eq_ignore_ascii_case("true") {
        Ok(())
    } else {
        bail!(
            "commit signing is off in {}; set commit.gpgsign so the lane's commits are signed",
            dir.display()
        )
    }
}

/// Compare this binary's release with the manifest's pin. A pin newer than
/// the running CLI is a refusal: a lane run by yesterday's binary cannot be
/// trusted with today's gate.
pub(crate) fn check_version_pin(pin: Option<&str>, binary: &str) -> Result<String> {
    let Some(pin) = pin.map(str::trim).filter(|pin| !pin.is_empty()) else {
        return Ok("no CLI version pin in the manifest".to_string());
    };
    let pinned = semver::Version::parse(pin)
        .with_context(|| format!("the manifest's version pin {pin:?} is not a semver version"))?;
    let mine = semver::Version::parse(binary)
        .with_context(|| format!("this binary's version {binary:?} is not a semver version"))?;
    if pinned > mine {
        bail!("this CLI is {mine} but the manifest pins {pinned}; upgrade the CLI and re-run");
    }
    Ok(format!(
        "this CLI release {mine} satisfies the manifest pin {pinned}"
    ))
}

/// The branch name the verb derives from a title when `--branch` is omitted:
/// `push/<date>-<slug>`, at most five slug words long, so two runs of the
/// verb on the same change on the same day share one branch.
pub(crate) fn branch_slug(title: &str, today: chrono::NaiveDate) -> String {
    let words: Vec<String> = title
        .split_whitespace()
        .take(5)
        .map(slug_word)
        .filter(|word| !word.is_empty())
        .collect();
    let slug = if words.is_empty() {
        "change".to_string()
    } else {
        words.join("-")
    };
    format!("push/{today}-{slug}")
}

fn slug_word(word: &str) -> String {
    let mut out = String::new();
    for character in word.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Watch the required check to a verdict (LAW-220 step 4), naming each stall
/// separately — failed check, held review, branch behind — then capture the
/// merge commit (step 5). Green without a merge yet is the ordinary path:
/// the arm takes a moment to squash.
pub(crate) async fn watch_check<S, Fut>(
    forge: &dyn Forge,
    review: &ReviewRef,
    sleep: S,
) -> Result<String>
where
    S: Fn(Duration) -> Fut,
    Fut: Future<Output = ()>,
{
    let deadline = Instant::now() + CHECK_TIMEOUT;
    loop {
        let verdict = forge.check_verdict(review).await?;
        if let Some(stall) = check_stall(&verdict) {
            bail!("{stall}");
        }
        if let CheckVerdict::Green = verdict {
            return merge_commit_within(forge, review, deadline, sleep).await;
        }
        if Instant::now() >= deadline {
            bail!(
                "the required check did not reach a verdict within {} seconds",
                CHECK_TIMEOUT.as_secs()
            );
        }
        sleep(CHECK_POLL).await;
    }
}

/// The stall a non-green, non-pending verdict names, so the watch loop and
/// the merge wait name it identically.
fn check_stall(verdict: &CheckVerdict) -> Option<String> {
    match verdict {
        CheckVerdict::Failed(name) => Some(format!(
            "required check {name} failed; fix it and re-run the verb on the same branch"
        )),
        CheckVerdict::BranchBehind => Some(
            "the review request is behind its base; rebase the branch and re-run the verb"
                .to_string(),
        ),
        CheckVerdict::HeldReview => Some(
            "the review request is held for review; resolve the review and re-run the verb"
                .to_string(),
        ),
        CheckVerdict::Conflict => Some(
            "the review request has a merge conflict; rebase the branch and re-run the verb"
                .to_string(),
        ),
        CheckVerdict::Green | CheckVerdict::Pending => None,
    }
}

/// Poll for the merge commit until the forge reports one or the lane's clock
/// runs out. The verdict keeps being polled while the merge is awaited: the
/// arm reports the merge on a later poll than the green one, and a re-run can
/// flip the check back to a stall. A merged-but-unreported window is a cache,
/// not a verdict.
async fn merge_commit_within<S, Fut>(
    forge: &dyn Forge,
    review: &ReviewRef,
    deadline: Instant,
    sleep: S,
) -> Result<String>
where
    S: Fn(Duration) -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        let verdict = forge.check_verdict(review).await?;
        if let Some(stall) = check_stall(&verdict) {
            bail!("{stall}");
        }
        if let CheckVerdict::Pending = verdict {
            if Instant::now() >= deadline {
                bail!(
                    "the required check did not reach a verdict within {} seconds",
                    CHECK_TIMEOUT.as_secs()
                );
            }
            sleep(CHECK_POLL).await;
            continue;
        }
        if let Some(sha) = forge.merge_commit(review).await? {
            return Ok(sha);
        }
        if Instant::now() >= deadline {
            bail!(
                "the review request did not merge within {} seconds of a green check",
                CHECK_TIMEOUT.as_secs()
            );
        }
        sleep(CHECK_POLL).await;
    }
}

/// The structured report (LAW-220 step 7): review URL, merge commit, and
/// where the required check landed.
pub(crate) fn render_report(report: &LaneReport) -> String {
    format!(
        "Landed.\n  review request: {}\n  merge commit: {}\n  required check: green",
        report.review_url, report.merge_commit
    )
}

#[cfg(test)]
mod tests {
    use super::{branch_slug, check_signing, check_version_pin, render_report, watch_check};
    use crate::projects::forge::{ArmOutcome, CheckVerdict, Forge, ReviewRef, ReviewRequest};
    use anyhow::Result;
    use async_trait::async_trait;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Mutex;
    use std::time::Instant;

    /// A forge that answers from a script, so the watch loop and the verb
    /// run against a mocked backend and no network.
    struct MockForge {
        review: ReviewRef,
        arm: ArmOutcome,
        /// The verdict every poll answers with; the verb reaches a verdict
        /// immediately or names the stall, so a static answer covers both.
        check: CheckVerdict,
        polls: Mutex<usize>,
        /// How many polls must pass before the merge is reported.
        merge_after_polls: usize,
        opened: Mutex<Vec<ReviewRequest>>,
    }

    impl MockForge {
        fn green_after(polls: usize) -> Self {
            Self {
                review: ReviewRef {
                    number: 7,
                    head_sha: "abc123".to_string(),
                    url: "https://forge.example/review/7".to_string(),
                },
                arm: ArmOutcome::Armed,
                check: CheckVerdict::Green,
                polls: Mutex::new(0),
                merge_after_polls: polls,
                opened: Mutex::new(Vec::new()),
            }
        }

        /// A forge whose required check answers the same stall forever.
        fn stuck(check: CheckVerdict) -> Self {
            let mut forge = Self::green_after(usize::MAX);
            forge.check = check;
            forge
        }
    }

    #[async_trait]
    impl Forge for MockForge {
        async fn identity(&self) -> Result<String> {
            Ok("mock".to_string())
        }

        async fn remote_reachable(&self) -> Result<()> {
            Ok(())
        }

        async fn open_review(&self, request: ReviewRequest) -> Result<ReviewRef> {
            self.opened.lock().unwrap().push(request);
            Ok(self.review.clone())
        }

        async fn arm_merge(&self, _review: &ReviewRef) -> Result<ArmOutcome> {
            Ok(self.arm.clone())
        }

        async fn check_verdict(&self, _review: &ReviewRef) -> Result<CheckVerdict> {
            let mut polls = self.polls.lock().unwrap();
            *polls += 1;
            Ok(self.check.clone())
        }

        async fn merge_commit(&self, _review: &ReviewRef) -> Result<Option<String>> {
            if *self.polls.lock().unwrap() >= self.merge_after_polls {
                Ok(Some("merge111".to_string()))
            } else {
                Ok(None)
            }
        }
    }

    async fn instant_sleep(_: std::time::Duration) {}

    #[tokio::test]
    async fn the_watch_names_a_failed_check_by_its_forged_name() {
        let forge = MockForge::stuck(CheckVerdict::Failed("ci / ci".to_string()));
        let error = watch_check(&forge, &forge.review, instant_sleep)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("ci / ci"), "{error:#}");
    }

    #[tokio::test]
    async fn the_watch_names_a_held_review_its_own_stall() {
        let forge = MockForge::stuck(CheckVerdict::HeldReview);
        let error = watch_check(&forge, &forge.review, instant_sleep)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("held for review"), "{error:#}");
    }

    #[tokio::test]
    async fn the_watch_names_a_branch_behind_its_own_stall() {
        let forge = MockForge::stuck(CheckVerdict::BranchBehind);
        let error = watch_check(&forge, &forge.review, instant_sleep)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("behind its base"), "{error:#}");
    }

    #[tokio::test]
    async fn the_watch_names_a_merge_conflict_its_own_stall() {
        let forge = MockForge::stuck(CheckVerdict::Conflict);
        let error = watch_check(&forge, &forge.review, instant_sleep)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("merge conflict"), "{error:#}");
    }

    #[tokio::test]
    async fn the_watch_polls_pending_then_captures_the_merge_commit() {
        // Green on the first poll, but the merge is only reported on the
        // second: the arm squashes a moment after the check goes green.
        let forge = MockForge::green_after(2);
        let sha = watch_check(&forge, &forge.review, instant_sleep)
            .await
            .unwrap();
        assert_eq!(sha, "merge111");
        assert_eq!(*forge.polls.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn the_watch_gives_up_when_the_merge_never_arrives() {
        let forge = MockForge::green_after(usize::MAX);
        let deadline = Instant::now();
        let error = super::merge_commit_within(&forge, &forge.review, deadline, instant_sleep)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("did not merge"), "{error:#}");
    }

    #[test]
    fn the_report_names_the_review_request_and_the_merge_commit() {
        let rendered = render_report(&super::LaneReport {
            review_url: "https://forge.example/review/7".to_string(),
            merge_commit: "merge111".to_string(),
        });
        assert!(rendered.contains("https://forge.example/review/7"));
        assert!(rendered.contains("merge111"));
        assert!(rendered.contains("required check: green"));
    }

    #[test]
    fn branch_slugs_come_from_the_title_and_the_date() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 30).unwrap();
        assert_eq!(
            branch_slug("Land this change, signed!", today),
            "push/2026-10-30-land-this-change-signed"
        );
        assert_eq!(
            branch_slug("One Two Three Four Five Six Seven", today),
            "push/2026-10-30-one-two-three-four-five"
        );
        assert_eq!(branch_slug("…", today), "push/2026-10-30-change");
    }

    #[test]
    fn the_version_pin_refuses_a_newer_manifest_and_accepts_the_rest() {
        assert!(check_version_pin(Some("99.0.0"), "26.10.30").is_err());
        assert!(check_version_pin(Some("26.10.30"), "26.10.30").is_ok());
        assert!(check_version_pin(Some("26.10.29"), "26.10.30").is_ok());
        assert!(check_version_pin(Some("not-a-version"), "26.10.30").is_err());
        assert!(check_version_pin(None, "26.10.30").is_ok());
        assert!(check_version_pin(Some("  "), "26.10.30").is_ok());
    }

    /// A throwaway Project repository with ssh-signed commits configured.
    /// The lane's own preflight requires signing, so its tests do too.
    struct TempCheckout {
        root: PathBuf,
    }

    impl TempCheckout {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap().keep();
            let repo = root.join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            git_sh(Some(&repo), &["init", "--initial-branch=main"]);
            git_sh(Some(&repo), &["config", "user.email", "agent@example.com"]);
            git_sh(Some(&repo), &["config", "user.name", "Agent"]);
            std::fs::write(
                repo.join("navigator.yaml"),
                format!("version: \"{}\"\n", env!("CARGO_PKG_VERSION")),
            )
            .unwrap();
            git_sh(Some(&repo), &["add", "-A"]);
            git_sh(Some(&repo), &["commit", "-m", "first"]);
            // The bare origin a push can reach with no network.
            git_sh(
                None,
                &["init", "--bare", root.join("origin.git").to_str().unwrap()],
            );
            git_sh(
                Some(&repo),
                &[
                    "remote",
                    "add",
                    "origin",
                    root.join("origin.git").to_str().unwrap(),
                ],
            );
            Self { root: repo }
        }

        fn with_signing(self) -> Self {
            let key = self.root.parent().unwrap().join("signing.key");
            let generated = Command::new("ssh-keygen")
                .args([
                    "-t",
                    "ed25519",
                    "-N",
                    "",
                    "-f",
                    key.to_str().unwrap(),
                    "-C",
                    "test",
                ])
                .output()
                .expect("ssh-keygen on the host");
            assert!(generated.status.success(), "ssh-keygen failed");
            // ssh-keygen names the public half `signing.key.pub`; `with_extension`
            // would replace the extension and point git at a nonexistent file.
            let public_key = PathBuf::from(format!("{}.pub", key.display()));
            git_sh(Some(&self.root), &["config", "gpg.format", "ssh"]);
            git_sh(
                Some(&self.root),
                &["config", "user.signingkey", public_key.to_str().unwrap()],
            );
            git_sh(Some(&self.root), &["config", "commit.gpgsign", "true"]);
            self
        }

        fn path(&self) -> &Path {
            &self.root
        }
    }

    fn git_sh(dir: Option<&Path>, args: &[&str]) {
        let mut command = Command::new("git");
        if let Some(dir) = dir {
            command.arg("-C").arg(dir);
        }
        let output = command.args(args).output().expect("git on the host");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn signing_preflight_holds_when_signing_is_off() {
        let checkout = TempCheckout::new();
        assert!(check_signing(checkout.path()).is_err());
    }

    #[test]
    fn signing_preflight_passes_when_signing_is_on() {
        let checkout = TempCheckout::new().with_signing();
        assert!(check_signing(checkout.path()).is_ok());
    }

    #[tokio::test]
    async fn the_verb_lands_a_change_to_a_report() {
        let checkout = TempCheckout::new().with_signing();
        std::fs::write(checkout.path().join("letter.md"), "# A change\n").unwrap();
        let forge = MockForge::green_after(1);
        let report = super::run(
            checkout.path(),
            "main",
            Some("push/the-change"),
            "the change",
            Some("body"),
            false,
            &forge,
            instant_sleep,
        )
        .await
        .unwrap();
        assert_eq!(report.merge_commit, "merge111");
        assert_eq!(report.review_url, forge.review.url);
        // The branch is on the bare origin under the name the verb chose.
        let head = Command::new("git")
            .arg("-C")
            .arg(checkout.path())
            .args(["rev-parse", "--verify", "origin/push/the-change"])
            .output()
            .unwrap();
        assert!(head.status.success(), "the branch was not pushed");
        // Two verdict polls: the watch loop's gate, then the merge wait's
        // re-poll (a merged-but-unreported window is a cache, not a verdict).
        assert_eq!(*forge.polls.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn a_dry_run_pushes_nothing() {
        let checkout = TempCheckout::new().with_signing();
        std::fs::write(checkout.path().join("letter.md"), "# A change\n").unwrap();
        let forge = MockForge::green_after(1);
        let error = super::run(
            checkout.path(),
            "main",
            Some("push/the-change"),
            "the change",
            None,
            true,
            &forge,
            instant_sleep,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("dry run"), "{error:#}");
        let branch = Command::new("git")
            .arg("-C")
            .arg(checkout.path())
            .args(["rev-parse", "--verify", "push/the-change"])
            .output()
            .unwrap();
        assert!(!branch.status.success(), "a dry run created a branch");
    }
}

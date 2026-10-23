//! The three-way decision behind `navigator project sync`.
//!
//! For every document the sync compares three things: the SHA-256 of the local
//! bytes, the revision the committed pointer names, and the live Project's
//! operative revision. [`decide`] turns those facts into one [`Decision`]; it
//! never touches a file or the network, so every row of the table is a plain
//! unit test. [`Approval`] is the other half: who may say yes to a decision that
//! overwrites a revision on either side.

use std::io::{BufRead, Write};

use anyhow::Result;
use uuid::Uuid;

/// The committed pointer's claim about one document.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PointerFact<'a> {
    pub(crate) asset_id: Uuid,
    pub(crate) sha256: &'a str,
}

/// The live Project's view of one document chain.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LiveFact<'a> {
    /// The newest revision: the one a lawyer reads and a new upload follows.
    pub(crate) operative: Uuid,
    pub(crate) sha256: &'a str,
    /// Every revision id on the chain, newest first.
    pub(crate) chain: &'a [Uuid],
}

/// Everything [`decide`] reads about one document.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Facts<'a> {
    /// SHA-256 of the local bytes; `None` when the file is absent.
    pub(crate) local: Option<&'a str>,
    pub(crate) pointer: Option<PointerFact<'a>>,
    pub(crate) live: Option<LiveFact<'a>>,
    /// The commit that removed this document's pointer from the repository,
    /// when the pointer existed before and is now gone.
    pub(crate) removed_in: Option<&'a str>,
}

/// What `project sync` does about one document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Local bytes, pointer, and live operative revision agree.
    Nothing,
    /// The bytes already match the live revision; only the pointer is written.
    RewritePointer,
    /// The local bytes are absent: fetch the live operative revision.
    Download,
    /// Asks: the local copy is an untouched older revision, so replace it with
    /// the newer live one.
    ReplaceLocal,
    /// Asks: the local bytes are an edit of the revision the pointer names, so
    /// upload them as a new revision of the same document.
    Upload,
    /// Asks which side wins: the local bytes were edited and the live document
    /// moved on. Never merged.
    Conflict,
    /// Asks once for the batch: a new file the live Project has no slug for.
    UploadNew,
    /// Asks, always individually: the repository removed this document's
    /// pointer, so the live document is soft-deleted for 180 days.
    SoftDelete,
    /// Neither side is touched. The reason is reported.
    CouldNotReconcile(&'static str),
}

/// The decision table of ENG-861.
pub(crate) fn decide(facts: &Facts<'_>) -> Decision {
    let Some(live) = facts.live else {
        return match (facts.pointer, facts.local) {
            (None, Some(_)) => Decision::UploadNew,
            (Some(_), _) => Decision::CouldNotReconcile(
                "the pointer names a document the live Project does not list",
            ),
            (None, None) => Decision::Nothing,
        };
    };
    let Some(pointer) = facts.pointer else {
        if facts.removed_in.is_some() {
            return Decision::SoftDelete;
        }
        return match facts.local {
            None => Decision::Download,
            Some(local) if local == live.sha256 => Decision::RewritePointer,
            // No pointer records which revision the bytes came from, so a
            // difference cannot be told from an edit: it is a conflict.
            Some(_) => Decision::Conflict,
        };
    };
    if !live.chain.contains(&pointer.asset_id) {
        return Decision::CouldNotReconcile(
            "the pointer names a revision the live Project does not hold",
        );
    }
    let pointer_is_operative = pointer.asset_id == live.operative;
    let Some(local) = facts.local else {
        return Decision::Download;
    };
    if local == live.sha256 {
        // The bytes are the live revision. A stale pointer is rewritten,
        // which also finishes a publication an earlier run began.
        return if pointer_is_operative {
            Decision::Nothing
        } else {
            Decision::RewritePointer
        };
    }
    match (local == pointer.sha256, pointer_is_operative) {
        (true, true) => Decision::Nothing,
        (true, false) => Decision::ReplaceLocal,
        (false, true) => Decision::Upload,
        (false, false) => Decision::Conflict,
    }
}

/// One answer to a yes/no prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    Yes,
    No,
    /// Yes to this and every remaining prompt of the same kind.
    All,
    Quit,
}

/// One answer to a which-side-wins prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Local,
    Live,
    Skip,
    Quit,
}

/// Where a prompt's answer comes from.
pub(crate) trait Prompter {
    fn confirm(&mut self, question: &str, offer_all: bool) -> Result<Answer>;
    fn choose_side(&mut self, question: &str, offer_local: bool) -> Result<Side>;
}

/// Answers typed at a terminal. Questions go to stderr so stdout keeps the
/// report.
pub(crate) struct TerminalPrompter<R, W> {
    input: R,
    output: W,
}

impl<R: BufRead, W: Write> TerminalPrompter<R, W> {
    pub(crate) fn new(input: R, output: W) -> Self {
        Self { input, output }
    }

    fn ask(&mut self, question: &str, choices: &str) -> Result<String> {
        write!(self.output, "{question} {choices} ")?;
        self.output.flush()?;
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            // A closed input is a quit, never a yes.
            return Ok("q".to_string());
        }
        Ok(line.trim().to_ascii_lowercase())
    }
}

impl<R: BufRead, W: Write> Prompter for TerminalPrompter<R, W> {
    fn confirm(&mut self, question: &str, offer_all: bool) -> Result<Answer> {
        let choices = if offer_all {
            "[y]es / [n]o / [a]ll remaining / [q]uit:"
        } else {
            "[y]es / [n]o / [q]uit:"
        };
        loop {
            match self.ask(question, choices)?.as_str() {
                "y" | "yes" => return Ok(Answer::Yes),
                "n" | "no" => return Ok(Answer::No),
                "a" | "all" if offer_all => return Ok(Answer::All),
                "q" | "quit" => return Ok(Answer::Quit),
                _ => {}
            }
        }
    }

    fn choose_side(&mut self, question: &str, offer_local: bool) -> Result<Side> {
        let choices = if offer_local {
            "[l]ocal wins / [r]emote (live) wins / [s]kip / [q]uit:"
        } else {
            "[r]emote (live) wins / [s]kip / [q]uit:"
        };
        loop {
            match self.ask(question, choices)?.as_str() {
                "l" | "local" if offer_local => return Ok(Side::Local),
                "r" | "remote" | "live" => return Ok(Side::Live),
                "s" | "skip" => return Ok(Side::Skip),
                "q" | "quit" => return Ok(Side::Quit),
                _ => {}
            }
        }
    }
}

/// How a decision that overwrites a revision gets its yes.
pub(crate) enum Approval {
    /// `--yes`: every yes/no is answered yes. A conflict and a deletion are not
    /// yes/no questions, so `--yes` does not answer them.
    Yes,
    /// A person at a terminal.
    Ask(Box<dyn Prompter>),
    /// No terminal and no `--yes`: nothing may be overwritten.
    Refuse,
}

/// The outcome of asking about one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolution {
    Apply,
    /// Apply the local side of a conflict.
    ApplyLocal,
    Decline,
    /// No one was in a position to say yes; the item stays as it was.
    Refused,
}

/// Remembers `a` and `q` across the prompts of one run.
pub(crate) struct Asker {
    approval: Approval,
    all: bool,
    quit: bool,
}

impl Asker {
    pub(crate) fn new(approval: Approval) -> Self {
        Self {
            approval,
            all: false,
            quit: false,
        }
    }

    /// A yes/no question. `a` answers it and every later one.
    pub(crate) fn confirm(&mut self, question: &str) -> Result<Resolution> {
        if self.quit {
            return Ok(Resolution::Decline);
        }
        match &mut self.approval {
            Approval::Yes => Ok(Resolution::Apply),
            Approval::Refuse => Ok(Resolution::Refused),
            Approval::Ask(_) if self.all => Ok(Resolution::Apply),
            Approval::Ask(prompter) => match prompter.confirm(question, true)? {
                Answer::Yes => Ok(Resolution::Apply),
                Answer::All => {
                    self.all = true;
                    Ok(Resolution::Apply)
                }
                Answer::No => Ok(Resolution::Decline),
                Answer::Quit => {
                    self.quit = true;
                    Ok(Resolution::Decline)
                }
            },
        }
    }

    /// A which-side question. `--yes` never picks a side.
    pub(crate) fn choose_side(&mut self, question: &str, offer_local: bool) -> Result<Resolution> {
        if self.quit {
            return Ok(Resolution::Decline);
        }
        match &mut self.approval {
            Approval::Yes | Approval::Refuse => Ok(Resolution::Refused),
            Approval::Ask(prompter) => match prompter.choose_side(question, offer_local)? {
                Side::Local => Ok(Resolution::ApplyLocal),
                Side::Live => Ok(Resolution::Apply),
                Side::Skip => Ok(Resolution::Decline),
                Side::Quit => {
                    self.quit = true;
                    Ok(Resolution::Decline)
                }
            },
        }
    }

    /// A deletion: always individual, never covered by `a`. `yes_delete` is
    /// the `--yes-delete` flag.
    pub(crate) fn confirm_deletion(
        &mut self,
        question: &str,
        yes_delete: bool,
    ) -> Result<Resolution> {
        if yes_delete {
            return Ok(Resolution::Apply);
        }
        if self.quit {
            return Ok(Resolution::Decline);
        }
        match &mut self.approval {
            Approval::Yes | Approval::Refuse => Ok(Resolution::Refused),
            Approval::Ask(prompter) => match prompter.confirm(question, false)? {
                Answer::Yes => Ok(Resolution::Apply),
                Answer::No => Ok(Resolution::Decline),
                Answer::Quit | Answer::All => {
                    self.quit = true;
                    Ok(Resolution::Decline)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const OLD: &str = "aaaaaaaa";
    const NEW: &str = "bbbbbbbb";
    const EDIT: &str = "cccccccc";

    struct Chain {
        old: Uuid,
        new: Uuid,
        both: Vec<Uuid>,
    }

    fn chain() -> Chain {
        let (old, new) = (Uuid::now_v7(), Uuid::now_v7());
        Chain {
            old,
            new,
            both: vec![new, old],
        }
    }

    fn live<'a>(chain: &'a Chain, sha256: &'a str) -> LiveFact<'a> {
        LiveFact {
            operative: chain.new,
            sha256,
            chain: &chain.both,
        }
    }

    fn pointer(asset_id: Uuid, sha256: &str) -> PointerFact<'_> {
        PointerFact { asset_id, sha256 }
    }

    /// Row 1: local bytes match the pointer, and the pointer is live.
    #[test]
    fn matching_bytes_on_the_operative_pointer_need_nothing() {
        let chain = chain();
        let facts = Facts {
            local: Some(NEW),
            pointer: Some(pointer(chain.new, NEW)),
            live: Some(live(&chain, NEW)),
            ..Facts::default()
        };
        assert_eq!(decide(&facts), Decision::Nothing);
    }

    /// Row 2: the bytes are absent and the pointer is live.
    #[test]
    fn absent_bytes_on_the_operative_pointer_download() {
        let chain = chain();
        let facts = Facts {
            local: None,
            pointer: Some(pointer(chain.new, NEW)),
            live: Some(live(&chain, NEW)),
            ..Facts::default()
        };
        assert_eq!(decide(&facts), Decision::Download);
    }

    /// Row 3: untouched bytes, but live has a newer revision.
    #[test]
    fn untouched_bytes_behind_a_newer_live_revision_ask_before_replacing() {
        let chain = chain();
        let facts = Facts {
            local: Some(OLD),
            pointer: Some(pointer(chain.old, OLD)),
            live: Some(live(&chain, NEW)),
            ..Facts::default()
        };
        assert_eq!(decide(&facts), Decision::ReplaceLocal);
    }

    /// Row 4: edited bytes, and the pointer is live.
    #[test]
    fn edited_bytes_on_the_operative_pointer_ask_to_upload_a_revision() {
        let chain = chain();
        let facts = Facts {
            local: Some(EDIT),
            pointer: Some(pointer(chain.new, NEW)),
            live: Some(live(&chain, NEW)),
            ..Facts::default()
        };
        assert_eq!(decide(&facts), Decision::Upload);
    }

    /// Row 5: edited bytes, and live moved on.
    #[test]
    fn edited_bytes_behind_a_newer_live_revision_conflict() {
        let chain = chain();
        let facts = Facts {
            local: Some(EDIT),
            pointer: Some(pointer(chain.old, OLD)),
            live: Some(live(&chain, NEW)),
            ..Facts::default()
        };
        assert_eq!(decide(&facts), Decision::Conflict);
    }

    /// Row 6: a new file with no pointer and no live slug.
    #[test]
    fn a_new_file_with_no_live_slug_uploads_as_a_new_document() {
        let facts = Facts {
            local: Some(EDIT),
            pointer: None,
            live: None,
            ..Facts::default()
        };
        assert_eq!(decide(&facts), Decision::UploadNew);
    }

    /// Row 7: no pointer, but the live document exists.
    #[test]
    fn a_live_document_with_no_pointer_downloads() {
        let chain = chain();
        let facts = Facts {
            local: None,
            pointer: None,
            live: Some(live(&chain, NEW)),
            ..Facts::default()
        };
        assert_eq!(decide(&facts), Decision::Download);
    }

    #[test]
    fn local_bytes_that_already_equal_the_live_revision_only_need_a_pointer() {
        let chain = chain();
        let no_pointer = Facts {
            local: Some(NEW),
            pointer: None,
            live: Some(live(&chain, NEW)),
            ..Facts::default()
        };
        assert_eq!(decide(&no_pointer), Decision::RewritePointer);
        // An earlier run uploaded these bytes and stopped before it wrote the
        // pointer: the pointer still names the previous revision.
        let interrupted = Facts {
            local: Some(NEW),
            pointer: Some(pointer(chain.old, OLD)),
            live: Some(live(&chain, NEW)),
            ..Facts::default()
        };
        assert_eq!(decide(&interrupted), Decision::RewritePointer);
    }

    #[test]
    fn differing_bytes_with_no_pointer_to_compare_are_a_conflict_not_a_download() {
        let chain = chain();
        let facts = Facts {
            local: Some(EDIT),
            pointer: None,
            live: Some(live(&chain, NEW)),
            ..Facts::default()
        };
        assert_eq!(decide(&facts), Decision::Conflict);
    }

    #[test]
    fn absent_bytes_behind_a_newer_live_revision_download_without_asking() {
        let chain = chain();
        let facts = Facts {
            local: None,
            pointer: Some(pointer(chain.old, OLD)),
            live: Some(live(&chain, NEW)),
            ..Facts::default()
        };
        assert_eq!(decide(&facts), Decision::Download);
    }

    #[test]
    fn a_pointer_the_live_project_does_not_hold_is_reported_and_left_alone() {
        let chain = chain();
        let stranger = Uuid::now_v7();
        for local in [None, Some(EDIT)] {
            let facts = Facts {
                local,
                pointer: Some(pointer(stranger, OLD)),
                live: Some(live(&chain, NEW)),
                ..Facts::default()
            };
            assert!(matches!(decide(&facts), Decision::CouldNotReconcile(_)));
        }
        let orphan = Facts {
            local: Some(EDIT),
            pointer: Some(pointer(stranger, OLD)),
            live: None,
            ..Facts::default()
        };
        assert!(matches!(decide(&orphan), Decision::CouldNotReconcile(_)));
    }

    fn terminal(script: &str) -> Asker {
        Asker::new(Approval::Ask(Box::new(TerminalPrompter::new(
            Cursor::new(script.to_string()),
            Vec::new(),
        ))))
    }

    #[test]
    fn a_terminal_answers_each_item_and_all_covers_the_rest() {
        let mut asker = terminal("n\na\n");
        assert_eq!(asker.confirm("one?").unwrap(), Resolution::Decline);
        assert_eq!(asker.confirm("two?").unwrap(), Resolution::Apply);
        assert_eq!(asker.confirm("three?").unwrap(), Resolution::Apply);
    }

    #[test]
    fn quit_declines_everything_after_it_and_a_closed_input_is_a_quit() {
        let mut asker = terminal("y\nq\n");
        assert_eq!(asker.confirm("one?").unwrap(), Resolution::Apply);
        assert_eq!(asker.confirm("two?").unwrap(), Resolution::Decline);
        assert_eq!(asker.confirm("three?").unwrap(), Resolution::Decline);
        assert_eq!(
            terminal("").confirm("one?").unwrap(),
            Resolution::Decline,
            "end of input must never read as a yes"
        );
    }

    #[test]
    fn yes_answers_yes_no_questions_but_never_picks_a_side() {
        let mut asker = Asker::new(Approval::Yes);
        assert_eq!(asker.confirm("overwrite?").unwrap(), Resolution::Apply);
        assert_eq!(
            asker.choose_side("which?", true).unwrap(),
            Resolution::Refused
        );
    }

    /// The facts of a removed pointer decide a soft deletion whatever the
    /// local bytes are, and never when a pointer is still there.
    #[test]
    fn a_removed_pointer_asks_to_soft_delete_the_live_document() {
        let chain = chain();
        for local in [None, Some(NEW), Some(EDIT)] {
            let facts = Facts {
                local,
                live: Some(live(&chain, NEW)),
                removed_in: Some("abc1234"),
                ..Facts::default()
            };
            assert_eq!(decide(&facts), Decision::SoftDelete, "{local:?}");
        }
        let still_there = Facts {
            local: Some(NEW),
            pointer: Some(pointer(chain.new, NEW)),
            live: Some(live(&chain, NEW)),
            removed_in: Some("abc1234"),
        };
        assert_eq!(decide(&still_there), Decision::Nothing);
    }

    #[test]
    fn yes_does_not_delete_but_yes_delete_does() {
        let mut asker = Asker::new(Approval::Yes);
        assert_eq!(
            asker.confirm_deletion("delete?", false).unwrap(),
            Resolution::Refused,
            "--yes covers overwrites, never deletions"
        );
        assert_eq!(
            asker.confirm_deletion("delete?", true).unwrap(),
            Resolution::Apply
        );
        assert_eq!(
            Asker::new(Approval::Refuse)
                .confirm_deletion("delete?", false)
                .unwrap(),
            Resolution::Refused,
            "no terminal and no --yes-delete refuses"
        );
        assert_eq!(
            Asker::new(Approval::Refuse)
                .confirm_deletion("delete?", true)
                .unwrap(),
            Resolution::Apply
        );
    }

    /// `a` is not a deletion answer: each deletion is its own question.
    #[test]
    fn all_is_not_offered_for_a_deletion() {
        let mut asker = terminal("a\ny\nn\n");
        assert_eq!(
            asker.confirm_deletion("one?", false).unwrap(),
            Resolution::Apply,
            "`a` is ignored and the prompt repeats until y or n"
        );
        assert_eq!(
            asker.confirm_deletion("two?", false).unwrap(),
            Resolution::Decline,
            "a yes to one deletion does not cover the next"
        );
    }

    #[test]
    fn no_terminal_and_no_yes_refuses() {
        let mut asker = Asker::new(Approval::Refuse);
        assert_eq!(asker.confirm("overwrite?").unwrap(), Resolution::Refused);
        assert_eq!(
            asker.choose_side("which?", true).unwrap(),
            Resolution::Refused
        );
    }

    #[test]
    fn a_conflict_prompt_returns_the_side_the_person_chose() {
        assert_eq!(
            terminal("l\n").choose_side("which?", true).unwrap(),
            Resolution::ApplyLocal
        );
        assert_eq!(
            terminal("r\n").choose_side("which?", true).unwrap(),
            Resolution::Apply
        );
        assert_eq!(
            terminal("s\n").choose_side("which?", true).unwrap(),
            Resolution::Decline
        );
    }
}

//! End-to-end import tests: walk the `templates/` fixture tree, lint
//! the markdown notation templates, and persist into a per-test
//! store spun up via `store::surreal::test_support::mem`. These
//! tests prove that
//!
//! 1. Every shipped template lints clean against the full Neon Law Navigator
//!    default rule set (so the fixtures stay honest).
//! 2. The import path actually writes templates and questions to the
//!    database — re-running it is idempotent and doesn't duplicate.
//! 3. Question codes referenced by `questionnaire:` and `workflow:`
//!    end up as `questions` rows the application can later resolve.

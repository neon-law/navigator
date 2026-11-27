//! App initialization shared by the project-notation HTTP contract tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use portal::{AppState, SessionStore};
use store::seed;

use workflows::{InMemoryRuntime, StateMachineRuntime};

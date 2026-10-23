//! Preview persistence is separate from questionnaire rendering and submission.
//! A server adapter must use authenticated, Project-scoped APIs; restoring a
//! browser snapshot must never submit intake or advance a Restate workflow.

use std::collections::BTreeMap;

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct PreviewSnapshot {
    pub template: String,
    pub step: usize,
    pub answers: BTreeMap<String, String>,
}

/// Storage adapters own I/O; the snapshot codec owns template compatibility.
trait SnapshotStorage {
    fn read(&self) -> Option<String>;
    fn write(&self, value: &str) -> bool;
}

#[derive(Default, Clone)]
pub(crate) struct PreviewStorage {
    #[cfg(target_arch = "wasm32")]
    browser: Option<(web_sys::Storage, String)>,
}

#[cfg(any(target_arch = "wasm32", test))]
fn storage_key(path: &str) -> Option<String> {
    path.strip_prefix("/notations/")
        .filter(|slug| !slug.is_empty())
        .map(|_| format!("navigator:/notations:v1:{path}"))
}

fn decode(value: &str, template: &str) -> Option<PreviewSnapshot> {
    let snapshot: PreviewSnapshot = serde_json::from_str(value).ok()?;
    (snapshot.template == template).then_some(snapshot)
}

impl PreviewStorage {
    pub fn for_current_page() -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            let browser = web_sys::window().and_then(|window| {
                let key = storage_key(&window.location().pathname().ok()?)?;
                Some((window.local_storage().ok()??, key))
            });
            Self { browser }
        }
        #[cfg(not(target_arch = "wasm32"))]
        Self::default()
    }

    pub fn load(&self, template: &str) -> Option<PreviewSnapshot> {
        decode(&self.read()?, template)
    }

    pub fn save(&self, snapshot: &PreviewSnapshot) -> bool {
        serde_json::to_string(snapshot).is_ok_and(|value| self.write(&value))
    }
}

impl SnapshotStorage for PreviewStorage {
    fn read(&self) -> Option<String> {
        #[cfg(target_arch = "wasm32")]
        {
            let (storage, key) = self.browser.as_ref()?;
            storage.get_item(key).ok()?
        }
        #[cfg(not(target_arch = "wasm32"))]
        None
    }

    fn write(&self, value: &str) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            self.browser
                .as_ref()
                .is_some_and(|(storage, key)| storage.set_item(key, value).is_ok())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = value;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_is_versioned_and_isolated_by_notation_path() {
        let letter = storage_key("/notations/onboarding-letter").expect("notation");
        assert_ne!(Some(letter), storage_key("/notations/privacy-policy"));
        assert!(storage_key("/app/projects/sample/intake").is_none());
        assert!(storage_key("/notations").is_none());
        assert!(storage_key("/notations/").is_none());
    }

    #[test]
    fn restores_answers_and_step_only_for_the_same_template() {
        let snapshot = PreviewSnapshot {
            template: "Hello {{person__client.name}}".into(),
            step: 3,
            answers: BTreeMap::from([("person__client".into(), "Acme".into())]),
        };
        let value = serde_json::to_string(&snapshot).expect("snapshot");
        let restored = decode(&value, &snapshot.template).expect("matching template");
        assert_eq!(restored.step, 3);
        assert_eq!(restored.answers, snapshot.answers);
        assert!(decode(&value, "revised template").is_none());
        assert!(decode("broken JSON", &snapshot.template).is_none());
    }
}

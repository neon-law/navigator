use super::{registered_adapters, EnqueueRecord, RunReport};
use crate::scraper::adapter::{find_adapter, SiteKey};
use crate::scraper::testing::MockAdapter;

fn site() -> SiteKey {
    SiteKey::new("la-superior").expect("valid key")
}

#[test]
fn the_registry_looks_adapters_up_by_site_key() {
    let binding = registered_adapters();
    let real = find_adapter(&binding, &site()).expect("the la-superior adapter is registered");
    assert_eq!(real.site(), &site());
    let pacer = SiteKey::new(crate::scraper::adapters::pacer::SITE_KEY).expect("valid key");
    let pacer_real = find_adapter(&binding, &pacer).expect("the pacer adapter is registered");
    assert_eq!(pacer_real.site(), &pacer);
    assert_eq!(pacer_real.credential_key(), "pacer");
    assert!(pacer_real.requires_login(), "PACER runs behind the account");
    let adapters: Vec<Box<dyn crate::scraper::adapter::SiteAdapter>> =
        vec![Box::new(MockAdapter::simple(site()))];
    let found = find_adapter(&adapters, &site()).expect("the mock is found");
    assert_eq!(found.site(), &site());
    assert!(find_adapter(&adapters, &SiteKey::new("pacer").expect("valid key")).is_none());
}

#[tokio::test]
async fn a_run_report_prints_its_transcript() {
    let report = RunReport {
        site: "la-superior".to_owned(),
        docket: "0004".to_owned(),
        since: None,
        discovered: 1,
        downloaded: 1,
        resumed: 0,
        skipped_completed: 0,
        skipped_filed: 0,
        enqueued: vec![EnqueueRecord {
            key: "la-superior/0004/4".to_owned(),
            message_id: uuid::Uuid::now_v7(),
            deduped: false,
            resumed: false,
        }],
        bytes: 5,
    };
    let rendered = format!("{report}");
    assert!(rendered.contains("scrape of la-superior docket 0004"));
    assert!(rendered.contains("discovered 1; downloaded 1"));
    assert!(rendered.contains("la-superior/0004/4"));
    assert!(rendered.contains("5 byte(s) staged"));
}

#[test]
fn run_state_serializes_across_processes() {
    let key = crate::scraper::adapter::NoticeKey::new(&site(), "0004", "12");
    let mut state = super::RunState::default();
    assert!(!state.contains(&key));
    state.mark(key.clone());
    assert!(state.contains(&key));
    let path = std::env::temp_dir().join(format!("navigator-run-state-{}", uuid::Uuid::now_v7()));
    state.save(&path).expect("save run state");
    let reloaded = super::RunState::load(&path).expect("load run state");
    assert_eq!(reloaded, state);
    std::fs::remove_file(&path).expect("clean up the temp file");
}

#[test]
fn a_missing_run_state_file_is_a_fresh_run() {
    let path = std::env::temp_dir().join(format!("navigator-run-state-{}", uuid::Uuid::now_v7()));
    let state = super::RunState::load(&path).expect("a missing file is empty");
    assert_eq!(state, super::RunState::default());
}

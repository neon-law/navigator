//! Private retained-object capture and canonical validation. No payload is logged.

use std::collections::{BTreeMap, BTreeSet};

use crate::{QuestionnaireSpec, StateName, WorkflowSpec};
use serde::{Deserialize, Serialize};
use store::durable_object_handover::{Capture, ClientViewDisposition, SlackProject};
use store::notation_machine_state::MachineState;
use uuid::Uuid;

/// Source capture validation failure; captured values are never included.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct CaptureError(pub &'static str);

type Result<T> = std::result::Result<T, CaptureError>;

const SERVICES: &str = "SELECT name,revision,deployment_id FROM sys_service WHERE name IN ('notation','project-slack') ORDER BY name LIMIT 3";
const STATE: &str = "SELECT service_name,service_key,key,value_utf8,scope FROM state WHERE service_name IN ('notation','project-slack') ORDER BY service_name,service_key,key LIMIT 100001";
const INVOCATIONS: &str = "SELECT i.id,i.target_service_name,i.target_service_key,i.target_handler_name,i.status,i.completion_result,i.scope,i.pinned_deployment_id,j.version,j.entry_json FROM sys_invocation i LEFT JOIN sys_journal j ON i.id=j.id AND j.index=0 WHERE i.target_service_name IN ('notation','project-slack') ORDER BY i.id LIMIT 100001";
const STATE_COUNT: &str =
    "SELECT COUNT(*) AS count FROM state WHERE service_name IN ('notation','project-slack')";
const INVOCATION_COUNT: &str = "SELECT COUNT(*) AS count FROM sys_invocation WHERE target_service_name IN ('notation','project-slack')";

const START_JOURNAL: &str = "SELECT j.id,j.index,j.version,j.entry_json FROM sys_journal j JOIN sys_invocation i ON i.id=j.id WHERE i.target_service_name='notation' AND i.target_handler_name IN ('questionnaire_start','workflow_start') ORDER BY j.id,j.index LIMIT 100001";
const START_JOURNAL_COUNT: &str = "SELECT COUNT(*) AS count FROM sys_journal j JOIN sys_invocation i ON i.id=j.id WHERE i.target_service_name='notation' AND i.target_handler_name IN ('questionnaire_start','workflow_start')";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalRow {
    id: String,
    index: u64,
    version: u32,
    entry_json: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Service {
    name: String,
    revision: u64,
    deployment_id: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    service_name: String,
    service_key: String,
    key: String,
    value_utf8: String,
    scope: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invocation {
    id: String,
    target_service_name: String,
    target_service_key: String,
    target_handler_name: String,
    status: String,
    completion_result: Option<String>,
    scope: Option<String>,
    pinned_deployment_id: Option<String>,
    version: Option<u32>,
    entry_json: Option<String>,
}

/// Exact source readback retained only in the private capture file.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Readback {
    services: Vec<Service>,
    state: Vec<State>,
    invocations: Vec<Invocation>,
    start_journal: Vec<JournalRow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Count {
    count: usize,
}

fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|_| CaptureError("unsupported object capture response"))
}

fn readback(mut query: impl FnMut(&str) -> Result<String>) -> Result<Readback> {
    let services: Vec<Service> = decode(&query(SERVICES)?)?;
    let state: Vec<State> = decode(&query(STATE)?)?;
    let invocations: Vec<Invocation> = decode(&query(INVOCATIONS)?)?;
    let start_journal: Vec<JournalRow> = decode(&query(START_JOURNAL)?)?;
    let journals: Vec<Count> = decode(&query(START_JOURNAL_COUNT)?)?;
    let states: Vec<Count> = decode(&query(STATE_COUNT)?)?;
    let calls: Vec<Count> = decode(&query(INVOCATION_COUNT)?)?;
    if services.len() != 2
        || services
            .iter()
            .map(|row| row.name.as_str())
            .collect::<BTreeSet<_>>()
            != BTreeSet::from(["notation", "project-slack"])
        || services.iter().any(|row| row.deployment_id.is_empty())
        || state.len() >= 100_001
        || invocations.len() >= 100_001
        || start_journal.len() >= 100_001
        || journals.len() != 1
        || journals[0].count != start_journal.len()
        || states.len() != 1
        || calls.len() != 1
        || states[0].count != state.len()
        || calls[0].count != invocations.len()
        || invocations.iter().any(|row| row.status != "completed")
    {
        return Err(CaptureError(
            "object capture is incomplete or has nonterminal work",
        ));
    }
    Ok(Readback {
        services,
        state,
        invocations,
        start_journal,
    })
}

pub fn check_environment(environment: &str) -> Result<()> {
    if environment.is_empty()
        || environment.len() > 128
        || !environment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CaptureError(
            "an explicit object source environment is required",
        ));
    }
    Ok(())
}

/// Read twice through the operator's explicitly configured authenticated source.
/// Matching readbacks bind a capture; the operator separately holds old writers.
pub fn frozen_readback_with(mut query: impl FnMut(&str) -> Result<String>) -> Result<Readback> {
    let first = readback(&mut query)?;
    let second = readback(&mut query)?;
    if first != second {
        return Err(CaptureError("object source changed between readbacks"));
    }
    Ok(second)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartInput {
    spec_yaml: String,
    // This is the actual captured start invocation's documented default,
    // never a default inferred from a missing business row or missing input.
    #[serde(default)]
    ephemeral: bool,
}

// Version 2 is serde's externally tagged Entry::Command(Command::Input).
// Pinned deployed-source definition: restatedev/restate v1.7.2,
// crates/types/src/journal_v2/{mod.rs:137,command.rs:45,147}.
#[derive(Deserialize)]
enum JournalEntry {
    Command(JournalCommand),
}
#[derive(Deserialize)]
enum JournalCommand {
    Input(JournalInput),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalInput {
    headers: Vec<serde_json::Value>,
    payload: Vec<u8>,
    name: String,
}

fn start_input(invocation: &Invocation) -> Result<StartInput> {
    if invocation.version != Some(2) {
        return Err(CaptureError(
            "object capture lacks supported start evidence",
        ));
    }
    let text = invocation
        .entry_json
        .as_deref()
        .ok_or(CaptureError("object capture lacks start evidence"))?;
    if text.len() > 6 * 1024 * 1024 {
        return Err(CaptureError("object start evidence exceeds bound"));
    }
    let JournalEntry::Command(JournalCommand::Input(input)) = decode(text)?;
    if input.payload.len() > 1024 * 1024 || input.name.len() > 1024 || input.headers.len() > 128 {
        return Err(CaptureError("object start evidence exceeds bound"));
    }
    serde_json::from_slice(&input.payload)
        .map_err(|_| CaptureError("unsupported object start evidence"))
}

// A successful repeated Start can return without initializing anything. Only
// the invocation that actually wrote this spec can ground its ephemeral setting.
// Pinned v1.7.2 journal_v2/command.rs SetStateCommand uses ByteString + Bytes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateWrite {
    key: String,
    value: Vec<u8>,
    name: String,
}

fn initialized_spec(
    source: &Readback,
    invocation: &Invocation,
    kind: &str,
    spec: &str,
) -> Result<bool> {
    let input = source
        .start_journal
        .iter()
        .filter(|row| row.id == invocation.id && row.index == 0)
        .collect::<Vec<_>>();
    if input.len() != 1 || invocation.entry_json.as_deref() != Some(input[0].entry_json.as_str()) {
        return Ok(false);
    }
    let mut writes = Vec::new();
    let mut positions = BTreeSet::new();
    for row in source
        .start_journal
        .iter()
        .filter(|row| row.id == invocation.id)
    {
        if row.version != 2
            || row.entry_json.len() > 6 * 1024 * 1024
            || !positions.insert(row.index)
        {
            return Err(CaptureError(
                "unsupported or duplicate start journal evidence",
            ));
        }
        let value: serde_json::Value = decode(&row.entry_json)?;
        if let Some(write) = value
            .get("Command")
            .and_then(|command| command.get("SetState"))
        {
            let write: StateWrite = serde_json::from_value(write.clone())
                .map_err(|_| CaptureError("unsupported start state-write evidence"))?;
            if write.value.len() > 1024 * 1024 || write.name.len() > 1024 {
                return Err(CaptureError("start state-write evidence exceeds bound"));
            }
            writes.push((write.key, write.value));
        }
    }
    let expected = [
        (format!("{kind}_spec_yaml"), spec),
        (format!("{kind}_state"), "BEGIN"),
    ];
    for (key, expected) in expected {
        let values: Vec<_> = writes
            .iter()
            .filter(|(written, _)| *written == key)
            .collect();
        if values.len() != 1 {
            return Ok(false);
        }
        let actual: String = serde_json::from_slice(&values[0].1)
            .map_err(|_| CaptureError("unsupported start state-write value"))?;
        if actual != expected {
            return Ok(false);
        }
    }
    Ok(true)
}

fn canonical_uuid(value: &str) -> Result<Uuid> {
    let id = Uuid::parse_str(value).map_err(|_| CaptureError("invalid object identity"))?;
    if id.is_nil() || id.to_string() != value {
        return Err(CaptureError("invalid object identity"));
    }
    Ok(id)
}

fn source_digest(source: &Readback) -> Result<String> {
    let bytes = serde_json::to_vec(source).map_err(|_| CaptureError("invalid object readback"))?;
    Ok(crate::attest::sha256_hex(&bytes))
}

/// Map actual retained keys and grounded start inputs into native rows.
/// A missing client-view receipt remains unresolved, even if a channel exists.
pub fn map_capture(
    source: &Readback,
    environment: &str,
    capture_id: Uuid,
    ownership_epoch: Uuid,
    project_inventory: &BTreeSet<Uuid>,
    captured_at_unix: u64,
) -> Result<Capture> {
    check_environment(environment)?;
    if source.services.len() != 2
        || source
            .services
            .iter()
            .map(|service| service.name.as_str())
            .collect::<BTreeSet<_>>()
            != BTreeSet::from(["notation", "project-slack"])
        || source
            .services
            .iter()
            .any(|service| service.deployment_id.is_empty())
        || source.state.len() >= 100_001
        || source.invocations.len() >= 100_001
    {
        return Err(CaptureError("incomplete captured object inventory"));
    }
    validate_captured_invocations(source)?;
    let mut objects: BTreeMap<Uuid, BTreeMap<String, String>> = BTreeMap::new();
    let mut projects: BTreeMap<Uuid, Vec<String>> = BTreeMap::new();
    for row in &source.state {
        if row.scope.is_some() || row.value_utf8.len() > 1024 * 1024 {
            return Err(CaptureError("unsupported object scope or value bound"));
        }
        let id = canonical_uuid(&row.service_key)?;
        match row.service_name.as_str() {
            "notation" => {
                if !matches!(
                    row.key.as_str(),
                    "questionnaire_spec_yaml"
                        | "questionnaire_state"
                        | "workflow_spec_yaml"
                        | "workflow_state"
                ) || objects
                    .entry(id)
                    .or_default()
                    .insert(row.key.clone(), row.value_utf8.clone())
                    .is_some()
                {
                    return Err(CaptureError("unknown or duplicate notation state key"));
                }
            }
            "project-slack" => {
                if row.key != "main_push_seen"
                    || !project_inventory.contains(&id)
                    || projects.contains_key(&id)
                {
                    return Err(CaptureError("unknown or duplicate Project Slack state key"));
                }
                projects.insert(id, decode(&row.value_utf8)?);
            }
            _ => return Err(CaptureError("unknown object service")),
        }
    }
    let machines = map_notation_machines(source, objects)?;
    let slack_projects = project_inventory
        .iter()
        .map(|id| {
            let posted = source.invocations.iter().any(|call| {
                call.target_service_name == "project-slack"
                    && call.target_service_key == id.to_string()
                    && call.target_handler_name == "client_project_view"
                    && call.completion_result.as_deref() == Some("success")
                    && source.services.iter().any(|service| {
                        service.name == "project-slack"
                            && call.pinned_deployment_id.as_deref()
                                == Some(service.deployment_id.as_str())
                    })
            });
            SlackProject {
                project_id: *id,
                main_push_seen: projects.remove(id).unwrap_or_default(),
                client_view: if posted {
                    ClientViewDisposition::Posted
                } else {
                    ClientViewDisposition::Unresolved
                },
            }
        })
        .collect();
    let capture = Capture {
        format_version: 1,
        capture_id,
        source_environment: environment.to_owned(),
        ownership_epoch,
        source_readback_sha256: source_digest(source)?,
        captured_at_unix,
        notation_machines: machines,
        slack_projects,
    };
    capture
        .validate()
        .map_err(|_| CaptureError("invalid captured object structure"))?;
    Ok(capture)
}

fn validate_captured_invocations(source: &Readback) -> Result<()> {
    let mut invocation_ids = BTreeSet::new();
    for call in &source.invocations {
        canonical_uuid(&call.target_service_key)?;
        let known_handler = match call.target_service_name.as_str() {
            "notation" => matches!(
                call.target_handler_name.as_str(),
                "questionnaire_start"
                    | "questionnaire_signal"
                    | "questionnaire_back"
                    | "questionnaire_current_state"
                    | "workflow_start"
                    | "workflow_signal"
                    | "workflow_current_state"
            ),
            "project-slack" => matches!(
                call.target_handler_name.as_str(),
                "client_project_view" | "main_push"
            ),
            _ => false,
        };
        if !known_handler
            || call.status != "completed"
            || call.scope.is_some()
            || !invocation_ids.insert(&call.id)
        {
            return Err(CaptureError(
                "unsupported or nonterminal captured object invocation",
            ));
        }
    }
    Ok(())
}

fn map_notation_machines(
    source: &Readback,
    objects: BTreeMap<Uuid, BTreeMap<String, String>>,
) -> Result<Vec<MachineState>> {
    let mut machines = Vec::new();
    for (id, keys) in objects {
        for kind in ["questionnaire", "workflow"] {
            let spec = keys.get(&format!("{kind}_spec_yaml"));
            let state = keys.get(&format!("{kind}_state"));
            let (Some(spec), Some(state)) = (spec, state) else {
                if spec.is_some() || state.is_some() {
                    return Err(CaptureError("incomplete notation machine state"));
                }
                continue;
            };
            let spec: String = decode(spec)?;
            let state: String = decode(state)?;
            let parsed = if kind == "questionnaire" {
                QuestionnaireSpec::from_yaml(&spec).map(|spec| spec.0)
            } else {
                WorkflowSpec::from_yaml(&spec)
            }
            .map_err(|_| CaptureError("invalid captured machine spec"))?;
            if !parsed.states.contains_key(&StateName(state.clone())) {
                return Err(CaptureError("captured state is outside its spec"));
            }
            let mut evidence = BTreeSet::new();
            for invocation in &source.invocations {
                if invocation.target_service_name == "notation"
                    && invocation.target_service_key == id.to_string()
                    && invocation.target_handler_name == format!("{kind}_start")
                    && invocation.completion_result.as_deref() == Some("success")
                    && source.services.iter().any(|service| {
                        service.name == "notation"
                            && invocation.pinned_deployment_id.as_deref()
                                == Some(service.deployment_id.as_str())
                    })
                {
                    let start = start_input(invocation)?;
                    if start.spec_yaml == spec && initialized_spec(source, invocation, kind, &spec)?
                    {
                        evidence.insert(start.ephemeral);
                    }
                }
            }
            if evidence.len() != 1 {
                return Err(CaptureError(
                    "missing or conflicting captured machine start evidence",
                ));
            }
            let ephemeral = *evidence
                .iter()
                .next()
                .ok_or(CaptureError("missing machine start evidence"))?;
            machines.push(MachineState {
                notation_id: id,
                machine_kind: kind.to_owned(),
                spec_yaml: spec,
                current_state: state,
                ephemeral,
            });
        }
    }
    Ok(machines)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "BEGIN:\n  _: END\nEND: {}\n";

    fn fixture() -> Readback {
        let notation = Uuid::from_u128(1).to_string();
        let project = Uuid::from_u128(2).to_string();
        let mut state = Vec::new();
        let mut invocations = Vec::new();
        let mut start_journal = Vec::new();
        for kind in ["questionnaire", "workflow"] {
            for (key, value) in [
                (
                    format!("{kind}_spec_yaml"),
                    serde_json::to_string(SPEC).unwrap(),
                ),
                (
                    format!("{kind}_state"),
                    serde_json::to_string("BEGIN").unwrap(),
                ),
            ] {
                state.push(State {
                    service_name: "notation".to_owned(),
                    service_key: notation.clone(),
                    key,
                    value_utf8: value,
                    scope: None,
                });
            }
            for (index, (key, value)) in [
                (format!("{kind}_spec_yaml"), SPEC),
                (format!("{kind}_state"), "BEGIN"),
            ]
            .into_iter()
            .enumerate()
            {
                start_journal.push(JournalRow { id: format!("synthetic-{kind}"), index: index as u64 + 2, version: 2, entry_json: serde_json::json!({"Command":{"SetState":{"key":key,"value":serde_json::to_vec(value).unwrap(),"name":""}}}).to_string() });
            }
            // This is the serde wire of the pinned source Entry::Command(Input):
            // enum wrappers, InputCommand fields and Bytes' JSON byte array.
            let payload =
                serde_json::to_vec(&serde_json::json!({"spec_yaml":SPEC,"ephemeral":true}))
                    .unwrap();
            invocations.push(Invocation {id:format!("synthetic-{kind}"),target_service_name:"notation".to_owned(),target_service_key:notation.clone(),target_handler_name:format!("{kind}_start"),status:"completed".to_owned(),completion_result:Some("success".to_owned()),scope:None,pinned_deployment_id:Some("synthetic-deployment".to_owned()),version:Some(2),entry_json:Some(serde_json::json!({"Command":{"Input":{"headers":[],"payload":payload,"name":""}}}).to_string())});
        }
        state.push(State {
            service_name: "project-slack".to_owned(),
            service_key: project.clone(),
            key: "main_push_seen".to_owned(),
            value_utf8: serde_json::to_string(&vec!["abc1234:1"]).unwrap(),
            scope: None,
        });
        invocations.push(Invocation {
            id: "synthetic-client-view".to_owned(),
            target_service_name: "project-slack".to_owned(),
            target_service_key: project,
            target_handler_name: "client_project_view".to_owned(),
            status: "completed".to_owned(),
            completion_result: Some("success".to_owned()),
            scope: None,
            pinned_deployment_id: Some("synthetic-deployment".to_owned()),
            version: None,
            entry_json: None,
        });
        for invocation in &invocations {
            if let Some(entry_json) = &invocation.entry_json {
                start_journal.push(JournalRow {
                    id: invocation.id.clone(),
                    index: 0,
                    version: 2,
                    entry_json: entry_json.clone(),
                });
            }
        }
        Readback {
            services: vec![
                Service {
                    name: "notation".to_owned(),
                    revision: 1,
                    deployment_id: "synthetic-deployment".to_owned(),
                },
                Service {
                    name: "project-slack".to_owned(),
                    revision: 1,
                    deployment_id: "synthetic-deployment".to_owned(),
                },
            ],
            state,
            invocations,
            start_journal,
        }
    }

    fn response(source: &Readback, query: &str) -> Result<String> {
        Ok(match query {
            SERVICES => serde_json::to_string(&source.services).unwrap(),
            STATE => serde_json::to_string(&source.state).unwrap(),
            INVOCATIONS => serde_json::to_string(&source.invocations).unwrap(),
            START_JOURNAL => serde_json::to_string(&source.start_journal).unwrap(),
            START_JOURNAL_COUNT => {
                serde_json::json!([{"count":source.start_journal.len()}]).to_string()
            }
            STATE_COUNT => serde_json::json!([{"count":source.state.len()}]).to_string(),
            INVOCATION_COUNT => serde_json::json!([{"count":source.invocations.len()}]).to_string(),
            _ => return Err(CaptureError("unexpected synthetic query")),
        })
    }

    fn mapped(source: &Readback) -> Result<Capture> {
        map_capture(
            source,
            "synthetic",
            Uuid::from_u128(3),
            Uuid::from_u128(4),
            &BTreeSet::from([Uuid::from_u128(2)]),
            1,
        )
    }

    #[test]
    fn actual_v2_input_mapping_preserves_two_machines_ephemeral_and_posted_history() {
        let fixture = fixture();
        let source = frozen_readback_with(|query| response(&fixture, query)).unwrap();
        let capture = mapped(&source).unwrap();
        assert_eq!(capture.notation_machines.len(), 2);
        assert!(capture
            .notation_machines
            .iter()
            .all(|machine| machine.ephemeral
                && machine.current_state == "BEGIN"
                && machine.spec_yaml == SPEC));
        assert_eq!(capture.slack_projects[0].main_push_seen, vec!["abc1234:1"]);
        assert!(capture.slack_projects[0].client_view == ClientViewDisposition::Posted);
        let mut no_view = source;
        no_view
            .invocations
            .retain(|row| row.target_service_name != "project-slack");
        assert!(
            mapped(&no_view).unwrap().slack_projects[0].client_view
                == ClientViewDisposition::Unresolved
        );
    }

    #[test]
    fn successful_noop_start_never_substitutes_for_expired_initializing_evidence() {
        let mut source = fixture();
        source.start_journal.clear();
        assert!(mapped(&source).is_err());
        source = fixture();
        let mut noop = source.invocations[0].clone();
        noop.id = "synthetic-later-noop".to_owned();
        let payload =
            serde_json::to_vec(&serde_json::json!({"spec_yaml":SPEC,"ephemeral":false})).unwrap();
        noop.entry_json = Some(
            serde_json::json!({"Command":{"Input":{"headers":[],"payload":payload,"name":""}}})
                .to_string(),
        );
        source.invocations.push(noop);
        assert!(mapped(&source)
            .unwrap()
            .notation_machines
            .iter()
            .all(|machine| machine.ephemeral));
        source
            .start_journal
            .retain(|row| row.id != "synthetic-questionnaire");
        assert!(mapped(&source).is_err());
    }

    #[test]
    fn mutating_source_and_truncated_inventories_refuse_capture() {
        let original = fixture();
        let mut reads = 0;
        assert!(frozen_readback_with(|query| {
            reads += 1;
            let mut changed = original.clone();
            if reads > 5 {
                changed.services[0].revision += 1;
            }
            response(&changed, query)
        })
        .is_err());
        assert!(frozen_readback_with(|query| {
            if query == STATE_COUNT {
                Ok("[{\"count\":999}]".to_owned())
            } else {
                response(&original, query)
            }
        })
        .is_err());
    }

    #[test]
    fn missing_start_evidence_unknown_keys_and_nonterminal_work_are_not_inferred() {
        let mut source = fixture();
        source.invocations[0].entry_json = None;
        assert!(mapped(&source).is_err());
        source = fixture();
        source.state[0].key = "unknown".to_owned();
        assert!(mapped(&source).is_err());
        source = fixture();
        source.invocations[0].status = "paused".to_owned();
        assert!(mapped(&source).is_err());
        source = fixture();
        source.state[0].scope = Some("unmapped".to_owned());
        assert!(mapped(&source).is_err());
        source = fixture();
        source.invocations[0].version = Some(1);
        assert!(mapped(&source).is_err());
    }
}

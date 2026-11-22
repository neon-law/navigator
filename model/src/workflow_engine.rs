//! Shared engine-selection metadata for workflow callers, startup, and deployment admission.

pub const COHOST_ENV: &str = "NAVIGATOR_WORKFLOWS_IN_WEB";
pub const ENGINE_ENV: &str = "NAVIGATOR_WORKFLOW_ENGINE";
pub const DBOS_ENABLED_ENV: &str = "NAVIGATOR_DBOS_ENABLED";

/// One registered workflow or object and the switch selecting its DBOS callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowSelection {
    pub name: &'static str,
    pub flag: &'static str,
}

/// Parent and tagged children share one ownership switch. The inbound alert and
/// triage rows also cover calls made indirectly by social reconciliation and polling.
pub const WORKFLOWS: &[WorkflowSelection] = &[
    WorkflowSelection {
        name: "EmailSummary",
        flag: "NAVIGATOR_DBOS_EMAIL_SUMMARY_ENABLED",
    },
    WorkflowSelection {
        name: "Archives",
        flag: "NAVIGATOR_DBOS_ARCHIVES_ENABLED",
    },
    WorkflowSelection {
        name: "DriDigest",
        flag: "NAVIGATOR_DBOS_DRI_DIGEST_ENABLED",
    },
    WorkflowSelection {
        name: "FinanceAggregate",
        flag: "NAVIGATOR_DBOS_FINANCE_AGGREGATE_ENABLED",
    },
    WorkflowSelection {
        name: "GeneralNag",
        flag: "NAVIGATOR_DBOS_GENERAL_NAG_ENABLED",
    },
    WorkflowSelection {
        name: "InboundMessageAlert",
        flag: "NAVIGATOR_DBOS_INBOUND_MESSAGE_ALERT_ENABLED",
    },
    WorkflowSelection {
        name: "InboundTriage",
        flag: "NAVIGATOR_DBOS_INBOUND_TRIAGE_ENABLED",
    },
    WorkflowSelection {
        name: "NightlyProjectUpdate",
        flag: "NAVIGATOR_DBOS_NIGHTLY_PROJECT_UPDATE_ENABLED",
    },
    WorkflowSelection {
        name: "NightlyProjectUpdateMembership",
        flag: "NAVIGATOR_DBOS_NIGHTLY_PROJECT_UPDATE_ENABLED",
    },
    WorkflowSelection {
        name: "NightlyProjectUpdateSlack",
        flag: "NAVIGATOR_DBOS_NIGHTLY_PROJECT_UPDATE_ENABLED",
    },
    WorkflowSelection {
        name: "NightlyProjectUpdateRepository",
        flag: "NAVIGATOR_DBOS_NIGHTLY_PROJECT_UPDATE_ENABLED",
    },
    WorkflowSelection {
        name: "NotationReset",
        flag: "NAVIGATOR_DBOS_NOTATION_RESET_ENABLED",
    },
    WorkflowSelection {
        name: "SocialPublisher",
        flag: "NAVIGATOR_DBOS_SOCIAL_PUBLISHER_ENABLED",
    },
    WorkflowSelection {
        name: "SocialPublishReconciler",
        flag: "NAVIGATOR_DBOS_SOCIAL_PUBLISH_RECONCILER_ENABLED",
    },
    WorkflowSelection {
        name: "YoutubeCommentsPoller",
        flag: "NAVIGATOR_DBOS_YOUTUBE_COMMENTS_POLLER_ENABLED",
    },
    WorkflowSelection {
        name: "notation",
        flag: "NAVIGATOR_DBOS_NOTATION_ENABLED",
    },
    WorkflowSelection {
        name: "project-slack",
        flag: "NAVIGATOR_DBOS_PROJECT_SLACK_ENABLED",
    },
];

/// Disabled billing automations are neither DBOS requirements nor Restate fallbacks.
/// Their names fence stale callers while retained executions receive operator disposition.
pub const DISABLED_BILLING: &[WorkflowSelection] = &[
    WorkflowSelection {
        name: "BillingCanary",
        flag: "NAVIGATOR_DBOS_BILLING_CANARY_ENABLED",
    },
    WorkflowSelection {
        name: "BillingDigest",
        flag: "NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED",
    },
    WorkflowSelection {
        name: "ReconcileInvoices",
        flag: "NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED",
    },
    WorkflowSelection {
        name: "SandboxCreditReset",
        flag: "NAVIGATOR_DBOS_SANDBOX_CREDIT_RESET_ENABLED",
    },
];

#[must_use]
pub fn disabled_billing_service(service: &str) -> bool {
    DISABLED_BILLING.iter().any(|row| row.name == service)
}

#[must_use]
pub fn disabled_billing_flag(flag: &str) -> bool {
    DISABLED_BILLING.iter().any(|row| row.flag == flag)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0} must select a supported workflow engine setting")]
pub struct SelectionError(pub &'static str);

fn enabled(
    get: &impl Fn(&str) -> Option<String>,
    key: &'static str,
) -> Result<bool, SelectionError> {
    match get(key).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(SelectionError(key)),
    }
}

/// Distinct caller switches that still select Restate. Invalid settings fail closed.
///
/// # Errors
/// Returns the name of a malformed selector, never its configuration value.
pub fn missing_dbos_flags(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Vec<&'static str>, SelectionError> {
    let mut missing = Vec::new();
    for selection in WORKFLOWS {
        if !enabled(get, selection.flag)? && !missing.contains(&selection.flag) {
            missing.push(selection.flag);
        }
    }
    Ok(missing)
}

/// Whether the complete cohost and every direct or indirect caller select DBOS.
/// Partial cohosting remains a mixed-engine topology; its Restate configuration
/// is still required. This says nothing about preserved or in-flight old work.
///
/// # Errors
/// Returns a malformed engine or boolean selector.
pub fn dbos_only(get: &impl Fn(&str) -> Option<String>) -> Result<bool, SelectionError> {
    let cohost = enabled(get, COHOST_ENV)?;
    let executor = dbos_executor(get)?;
    Ok(cohost && executor && missing_dbos_flags(get)?.is_empty())
}

/// Whether an actual DBOS executor is selected, without requiring cohosting or
/// migration of unrelated services. Invalid settings identify only their key.
///
/// # Errors
/// The executor or engine selector is malformed.
pub fn dbos_executor(get: &impl Fn(&str) -> Option<String>) -> Result<bool, SelectionError> {
    let dbos = enabled(get, DBOS_ENABLED_ENV)?;
    let engine = match get(ENGINE_ENV).as_deref().map(str::trim) {
        None | Some("" | "restate") => false,
        Some("dbos") => true,
        Some(_) => return Err(SelectionError(ENGINE_ENV)),
    };
    Ok(dbos && engine)
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_deployment_does_not_require_disabled_billing() {
        let values = native();
        for value in [None, Some("false"), Some("true"), Some("stale")] {
            let lookup = |key: &str| {
                if super::disabled_billing_flag(key) {
                    value.map(str::to_owned)
                } else {
                    values.get(key).cloned()
                }
            };
            assert!(super::dbos_only(&lookup).unwrap());
        }
    }

    #[test]
    fn selected_executor_does_not_require_unrelated_service_switches_or_cohosting() {
        let lookup = |key: &str| match key {
            super::ENGINE_ENV => Some("dbos".into()),
            super::DBOS_ENABLED_ENV => Some("true".into()),
            _ => None,
        };
        assert!(super::dbos_executor(&lookup).unwrap());
        assert!(!super::dbos_only(&lookup).unwrap());
        for (key, value) in [
            (super::ENGINE_ENV, None),
            (super::ENGINE_ENV, Some("restate")),
            (super::DBOS_ENABLED_ENV, None),
            (super::DBOS_ENABLED_ENV, Some("false")),
        ] {
            assert!(!super::dbos_executor(&|candidate| if candidate == key {
                value.map(str::to_owned)
            } else {
                lookup(candidate)
            })
            .unwrap());
        }
        for key in [super::ENGINE_ENV, super::DBOS_ENABLED_ENV] {
            assert_eq!(
                super::dbos_executor(&|candidate| if candidate == key {
                    Some("invalid".into())
                } else {
                    lookup(candidate)
                })
                .unwrap_err()
                .0,
                key
            );
        }
    }

    use super::*;
    use std::collections::BTreeMap;

    fn native() -> BTreeMap<&'static str, String> {
        let mut values: BTreeMap<_, _> = WORKFLOWS
            .iter()
            .map(|row| (row.flag, "true".into()))
            .collect();
        values.extend([
            (COHOST_ENV, "true".into()),
            (DBOS_ENABLED_ENV, "true".into()),
            (ENGINE_ENV, "dbos".into()),
        ]);
        values
    }

    #[test]
    fn complete_native_callers_require_the_actual_cohost_tuple() {
        assert!(!dbos_only(&|_| None).unwrap());
        let mut values = native();
        assert!(dbos_only(&|key| values.get(key).cloned()).unwrap());
        for key in [COHOST_ENV, DBOS_ENABLED_ENV, ENGINE_ENV] {
            let previous = values.remove(key).unwrap();
            assert!(!dbos_only(&|key| values.get(key).cloned()).unwrap());
            values.insert(key, previous);
        }
    }

    #[test]
    fn each_indirect_or_direct_restate_caller_keeps_mixed_engine_admission() {
        for row in WORKFLOWS {
            let mut values = native();
            values.insert(row.flag, "false".into());
            assert!(
                !dbos_only(&|key| values.get(key).cloned()).unwrap(),
                "{}",
                row.name
            );
            assert_eq!(
                missing_dbos_flags(&|key| values.get(key).cloned()).unwrap(),
                vec![row.flag]
            );
            values.insert(row.flag, "invalid".into());
            assert_eq!(
                dbos_only(&|key| values.get(key).cloned()),
                Err(SelectionError(row.flag))
            );
        }
    }
}

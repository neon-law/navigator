//! The [Metered Session](https://www.neonlaw.com/glossary/metered-session):
//! the ledger-gated completion path every Legal Sandbox exchange runs
//! through.
//!
//! [`ModelProvider`] deliberately carries no identity — sanitization is
//! the session layer's job — so the meter cannot live on the trait. A
//! [`MeteredSession`] binds one backend provider to one subscriber's
//! [Sandbox Usage](https://www.neonlaw.com/glossary/sandbox-usage) row
//! for one billing month and enforces the glossary's money rules on every
//! exchange:
//!
//! - **Checked before it runs.** A session already past its credit gets
//!   the à la carte offer ([`MeteredOutcome::Exhausted`]) and the model
//!   is never called.
//! - **Billed only on success.** A failed completion returns `Err` and
//!   debits nothing.
//! - **The crossing completion completes.** The exchange that pushes the
//!   balance negative runs, is billed in full, and flags the row
//!   `overage`; the next one waits for consent.
//!
//! The ledger itself is reached through the [`UsageLedger`] seam so the
//! session's decision logic is testable without an engine; [`StoreLedger`]
//! is the production adapter over `store::sandbox_usage`, whose
//! conditional debit is what makes two parallel tabs of the same
//! subscriber settle without a double-spend.

use std::sync::Arc;

use async_trait::async_trait;
use store::sandbox_usage::{self, MeterDecision, SandboxUsage, SandboxUsageError};
use store::surreal::SurrealDb;
use thiserror::Error;
use uuid::Uuid;

use crate::{CompletionRequest, CompletionResult, ModelProvider};

/// The subscriber's metered-credit ledger, as the metered session sees
/// it. A seam, not a type alias: the session's tests bind a deterministic
/// in-process fake, and the production [`StoreLedger`] delegates to
/// `store::sandbox_usage` — the module whose conditional update settles
/// parallel debits. Implementations enforce the ledger's own rules
/// (period spelling, negative prices) and report them in the error.
#[async_trait]
pub trait UsageLedger: Send + Sync {
    /// The subscriber's row for the period, or `None` before the first
    /// grant.
    ///
    /// # Errors
    ///
    /// Whatever the ledger reports for a failed read.
    async fn find(
        &self,
        person_id: Uuid,
        period: &str,
    ) -> Result<Option<SandboxUsage>, SandboxUsageError>;

    /// Charge one completion's price against the period's row.
    ///
    /// # Errors
    ///
    /// Whatever the ledger reports for a refused or failed charge.
    async fn debit(
        &self,
        person_id: Uuid,
        period: &str,
        price_cents: i64,
    ) -> Result<MeterDecision, SandboxUsageError>;
}

/// The production [`UsageLedger`]: every read and write lands in
/// `store::sandbox_usage`, the table whose single conditional `UPDATE`
/// is the concurrency boundary. Cloneable — `SurrealDb` is a handle.
#[derive(Clone)]
pub struct StoreLedger {
    db: SurrealDb,
}

impl StoreLedger {
    /// Wrap a store handle.
    #[must_use]
    pub fn new(db: SurrealDb) -> Self {
        Self { db }
    }
}

#[async_trait]
impl UsageLedger for StoreLedger {
    async fn find(
        &self,
        person_id: Uuid,
        period: &str,
    ) -> Result<Option<SandboxUsage>, SandboxUsageError> {
        sandbox_usage::find(&self.db, person_id, period).await
    }

    async fn debit(
        &self,
        person_id: Uuid,
        period: &str,
        price_cents: i64,
    ) -> Result<MeterDecision, SandboxUsageError> {
        sandbox_usage::debit(&self.db, person_id, period, price_cents).await
    }
}

/// What one metered exchange decided. The session surface the sandbox
/// page's credit states render from.
#[derive(Debug, Clone, PartialEq)]
pub enum MeteredOutcome {
    /// The completion ran and was billed. `spent_cents` /
    /// `remaining_cents` are the row's totals after the charge —
    /// negative `remaining_cents` on the charge that crossed, which is
    /// also the moment the row flags `overage`.
    Completed {
        /// The model's reply.
        result: CompletionResult,
        /// The row's total debit after this charge, in cents.
        spent_cents: i64,
        /// The balance after this charge, in cents.
        remaining_cents: i64,
    },
    /// The completion ran, but the ledger refused the charge: a parallel
    /// tab of the same subscriber crossed zero between this exchange's
    /// check and its debit. The reply is real — the model already
    /// answered — but nothing was billed; the next exchange waits for
    /// consent like any other overage. Rare by construction (one
    /// subscriber, two tabs, one crossing), and the honest alternative —
    /// reporting it billed — would be a meter that lies.
    Unbilled {
        /// The model's reply, delivered unbilled.
        result: CompletionResult,
    },
    /// The credit was already crossed before this exchange: the model
    /// was never called and the à la carte offer replaces the composer.
    Exhausted,
}

/// Errors a metered exchange can report.
#[derive(Debug, Error)]
pub enum MeteredError {
    /// The model backend failed. Nothing is debited — a failed
    /// completion is never charged.
    #[error("model provider: {0}")]
    Provider(#[source] anyhow::Error),
    /// The ledger refused or failed the charge.
    #[error("usage ledger: {0}")]
    Ledger(#[from] SandboxUsageError),
}

/// One subscriber's ledger-gated view of the [`ModelGateway`]: the
/// composition the sandbox routes hold per request. `provider` and
/// `ledger` are shared behind `Arc`; `subscriber` and `period` come from
/// the authenticated request, never from the model payload.
///
/// [`ModelGateway`]: https://www.neonlaw.com/glossary/model-gateway
pub struct MeteredSession {
    provider: Arc<dyn ModelProvider>,
    ledger: Arc<dyn UsageLedger>,
    subscriber: Uuid,
    period: String,
}

impl MeteredSession {
    /// Bind a backend provider and a ledger to one subscriber's one
    /// billing month.
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        ledger: Arc<dyn UsageLedger>,
        subscriber: Uuid,
        period: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            ledger,
            subscriber,
            period: period.into(),
        }
    }

    /// Run one exchange through the meter: check, complete, debit.
    ///
    /// # Errors
    ///
    /// [`MeteredError::Provider`] when the backend fails (nothing
    /// debited); [`MeteredError::Ledger`] when the ledger read or write
    /// fails. A crossed credit is not an error — it is
    /// [`MeteredOutcome::Exhausted`], a product state the composer
    /// renders.
    pub async fn complete(&self, req: CompletionRequest) -> Result<MeteredOutcome, MeteredError> {
        // Checked before it runs: a crossed row never reaches the model.
        // The overage flag — not `spent >= granted` — is the gate: a row
        // spent exactly to zero still has its crossing completion coming,
        // and that exchange runs and is billed in full.
        let Some(usage) = self
            .ledger
            .find(self.subscriber, &self.period)
            .await
            .map_err(MeteredError::Ledger)?
        else {
            // No grant yet: there is nothing to spend, which renders as
            // the same product state — the à la carte offer.
            return Ok(MeteredOutcome::Exhausted);
        };
        if usage.overage {
            return Ok(MeteredOutcome::Exhausted);
        }

        // Billed only on success: the debit happens strictly after a
        // successful reply, so a failed completion costs nothing.
        let result = self
            .provider
            .complete(req)
            .await
            .map_err(MeteredError::Provider)?;

        let price_cents = i64::try_from(self.provider.price_cents(&result))
            .map_err(|err| MeteredError::Provider(anyhow::Error::from(err)))?;
        match self
            .ledger
            .debit(self.subscriber, &self.period, price_cents)
            .await
        {
            Ok(MeterDecision::Charge {
                spent_cents,
                remaining_cents,
            }) => Ok(MeteredOutcome::Completed {
                result,
                spent_cents,
                remaining_cents,
            }),
            // A parallel tab crossed the credit between this exchange's
            // check and its debit: the reply is real and is delivered
            // unbilled, and the next exchange waits for consent like any
            // other overage.
            Ok(MeterDecision::Exhausted) => Ok(MeteredOutcome::Unbilled { result }),
            Err(err) => Err(MeteredError::Ledger(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use chrono::Utc;
    use store::sandbox_usage::{MeterDecision, SandboxUsage, SandboxUsageError};
    use uuid::Uuid;

    use super::{MeteredError, MeteredOutcome, MeteredSession, UsageLedger};
    use crate::{CompletionRequest, CompletionResult, Message, Role, StubModelProvider};

    const GRANT: i64 = 500;

    /// A subscriber nobody else's test knows.
    fn subscriber() -> Uuid {
        Uuid::now_v7()
    }

    /// A request the stub answers; contents are irrelevant to the meter.
    fn request() -> CompletionRequest {
        CompletionRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Is my lease assignable?".into(),
            }],
            ..CompletionRequest::default()
        }
    }

    /// A stub priced to report `price_cents` per completion: the whole
    /// price rides the input leg at the default 200¢ per million tokens.
    fn priced_provider(price_cents: u64) -> StubModelProvider {
        let mut provider = StubModelProvider::new();
        provider.set_usage(price_cents * 5_000, 0);
        provider
    }

    /// The metered session under test, over the fake ledger. The caller
    /// keeps the ledger handle to assert on the debits.
    fn session(provider: StubModelProvider, ledger: Arc<FakeLedger>, who: Uuid) -> MeteredSession {
        MeteredSession::new(Arc::new(provider), ledger, who, "2026-11")
    }

    /// An in-memory ledger mirroring `store::sandbox_usage`'s rules —
    /// the crossing debit, and the refusal once overage is set — plus a
    /// knob that forces every debit to report `Exhausted`, the shape of
    /// a parallel tab's crossing landing between this exchange's check
    /// and its debit.
    struct FakeLedger {
        granted_cents: i64,
        state: Mutex<FakeState>,
        debit_always_refused: bool,
    }

    struct FakeState {
        spent_cents: i64,
        overage: bool,
        /// Every `debit` price, in call order.
        debits: Vec<i64>,
    }

    impl FakeLedger {
        fn ledger() -> Arc<Self> {
            Arc::new(Self::with_refusal(false))
        }

        /// A ledger whose every debit reports `Exhausted` while `find`
        /// still reports a live row — the parallel-crossing race.
        fn refused() -> Arc<Self> {
            Arc::new(Self::with_refusal(true))
        }

        fn with_refusal(debit_always_refused: bool) -> Self {
            Self {
                granted_cents: GRANT,
                state: Mutex::new(FakeState {
                    spent_cents: 0,
                    overage: false,
                    debits: Vec::new(),
                }),
                debit_always_refused,
            }
        }

        /// Push the row past its credit, as a prior exchange would have.
        fn cross(&self, price: i64) {
            let mut state = self.state.lock().expect("fake ledger lock");
            state.spent_cents += price;
            state.overage = state.spent_cents > self.granted_cents;
        }

        fn state(&self) -> FakeState {
            self.state.lock().expect("fake ledger lock").clone()
        }
    }

    impl Clone for FakeState {
        fn clone(&self) -> Self {
            Self {
                spent_cents: self.spent_cents,
                overage: self.overage,
                debits: self.debits.clone(),
            }
        }
    }

    #[async_trait]
    impl UsageLedger for FakeLedger {
        async fn find(
            &self,
            _person_id: Uuid,
            _period: &str,
        ) -> Result<Option<SandboxUsage>, SandboxUsageError> {
            let state = self.state.lock().expect("fake ledger lock");
            Ok(Some(SandboxUsage {
                person_id: Uuid::nil(),
                period: "2026-11".into(),
                granted_cents: self.granted_cents,
                spent_cents: state.spent_cents,
                overage: state.overage,
                inserted_at: Utc::now(),
                updated_at: Utc::now(),
            }))
        }

        async fn debit(
            &self,
            _person_id: Uuid,
            _period: &str,
            price_cents: i64,
        ) -> Result<MeterDecision, SandboxUsageError> {
            let mut state = self.state.lock().expect("fake ledger lock");
            state.debits.push(price_cents);
            if self.debit_always_refused {
                return Ok(MeterDecision::Exhausted);
            }
            state.spent_cents += price_cents;
            state.overage = state.spent_cents > self.granted_cents;
            Ok(MeterDecision::Charge {
                spent_cents: state.spent_cents,
                remaining_cents: self.granted_cents - state.spent_cents,
            })
        }
    }

    /// `Completed`, unpacked — the assertions all want the fields.
    fn completed(outcome: MeteredOutcome) -> (CompletionResult, i64, i64) {
        match outcome {
            MeteredOutcome::Completed {
                result,
                spent_cents,
                remaining_cents,
            } => (result, spent_cents, remaining_cents),
            other => panic!("expected a completed exchange, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_fresh_session_bills_its_first_completion_against_the_grant() {
        let who = subscriber();
        let ledger = FakeLedger::ledger();
        let provider = priced_provider(125);
        let session = session(provider, Arc::clone(&ledger), who);

        let (result, spent, remaining) = completed(session.complete(request()).await.unwrap());

        assert_eq!(spent, 125);
        assert_eq!(remaining, GRANT - 125);
        assert_eq!(result.messages.len(), 1, "the reply reaches the subscriber");
        assert_eq!(
            ledger.state().debits,
            vec![125],
            "the meter debits the priced exchange"
        );
    }

    #[tokio::test]
    async fn a_session_past_its_credit_blocks_before_the_model_is_called() {
        let who = subscriber();
        let ledger = FakeLedger::ledger();
        ledger.cross(600);
        let provider = priced_provider(125);
        let session = session(provider, Arc::clone(&ledger), who);

        let outcome = session.complete(request()).await.unwrap();

        assert_eq!(outcome, MeteredOutcome::Exhausted);
        assert!(
            ledger.state().debits.is_empty(),
            "a blocked exchange debits nothing"
        );
    }

    #[tokio::test]
    async fn a_failed_completion_debits_nothing() {
        let who = subscriber();
        let ledger = FakeLedger::ledger();
        let mut provider = priced_provider(125);
        provider.set_failure(Some("vertex 503".into()));
        let session = session(provider, Arc::clone(&ledger), who);

        let error = session.complete(request()).await.unwrap_err();

        assert!(
            matches!(error, MeteredError::Provider(_)),
            "the backend failure surfaces, not a ledger error"
        );
        assert_eq!(
            ledger.state().debits,
            Vec::<i64>::new(),
            "a failed completion is never charged"
        );
    }

    #[tokio::test]
    async fn the_crossing_completion_completes_and_is_billed_in_full() {
        let who = subscriber();
        let ledger = FakeLedger::ledger();
        let session = session(priced_provider(600), Arc::clone(&ledger), who);

        let (_, spent, remaining) = completed(session.complete(request()).await.unwrap());

        assert_eq!(spent, 600, "the crossing exchange is billed in full");
        assert_eq!(remaining, -100);
        assert!(ledger.state().overage, "the crossing flags the row");
    }

    #[tokio::test]
    async fn spending_to_exactly_zero_leaves_one_more_completion() {
        let who = subscriber();
        let ledger = FakeLedger::ledger();
        let first = session(priced_provider(500), Arc::clone(&ledger), who);
        let (_, _, remaining) = completed(first.complete(request()).await.unwrap());
        assert_eq!(remaining, 0, "zero is not crossed");

        // One cent past the empty balance: the crossing exchange still
        // runs, completes, and is billed in full.
        let second = session(priced_provider(501), Arc::clone(&ledger), who);
        let (_, spent, remaining) = completed(second.complete(request()).await.unwrap());

        assert_eq!(
            spent,
            GRANT + 501,
            "the crossing exchange is billed in full"
        );
        assert_eq!(remaining, -501);
        assert!(ledger.state().overage, "the crossing flags the row");
    }

    #[tokio::test]
    async fn a_debit_refused_after_the_check_delivers_the_reply_unbilled() {
        let who = subscriber();
        let ledger = FakeLedger::refused();
        let session = session(priced_provider(125), Arc::clone(&ledger), who);

        let outcome = session.complete(request()).await.unwrap();

        match outcome {
            MeteredOutcome::Unbilled { result } => {
                assert_eq!(result.messages.len(), 1);
            }
            other => panic!("expected an unbilled reply, got {other:?}"),
        }
        assert_eq!(
            ledger.state().debits,
            vec![125],
            "the refused charge was attempted exactly once"
        );
    }
}

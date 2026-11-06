Feature: E-signature completion webhook closes the retainer loop
  A submitted retainer parks at sent_for_signature__pending until the
  e-signature provider reports the client has signed. The provider's
  completion callback is HMAC-verified over the raw body before it is
  allowed to advance workflow state — an unauthenticated POST here would
  be a state-advancing forgery (the firm asserting a client signed when
  they did not). The engagement terms were already attorney-reviewed at
  the lawyer_review gate before the document was sent, so signature
  receipt is a ministerial transition with no human in the middle.

  Background:
    Given a Neon Law Navigator app with an HMAC-secured e-signature webhook
    And a retainer parked at sent_for_signature__pending with envelope id "fake"

  Scenario: A verified completion callback advances the retainer to END
    When the provider posts a validly-signed completion callback for envelope "fake"
    Then the response status is 200
    And the retainer workflow has advanced to "END"
    And the notation row state is "END"

  Scenario: A verified decline callback records a declined terminal signature state
    When the provider posts a validly-signed decline callback for envelope "fake"
    Then the response status is 200
    And the retainer workflow has advanced to "END"
    And the notation row state is "END"
    And the signature row state is "declined"

  Scenario: A verified expiry callback records an expired terminal signature state
    When the provider posts a validly-signed expiry callback for envelope "fake"
    Then the response status is 200
    And the retainer workflow has advanced to "END"
    And the notation row state is "END"
    And the signature row state is "expired"

  Scenario: A later decline cannot regress a completed signature
    When the provider posts a validly-signed completion callback for envelope "fake"
    Then the response status is 200
    And the signature row state is "completed"
    When the provider posts a validly-signed decline callback for envelope "fake"
    Then the response status is 200
    And the signature row state is "completed"

  Scenario: A forged completion callback is rejected and the retainer stays pending
    When an attacker posts a completion callback with a forged signature for envelope "fake"
    Then the response status is 401
    And the retainer workflow is still at "sent_for_signature__pending"
    And the notation row state is "sent_for_signature__pending"

  Scenario: A viewed event is acknowledged and the retainer stays pending
    When the provider posts a validly-signed viewed callback for envelope "fake"
    Then the response status is 200
    And the retainer workflow is still at "sent_for_signature__pending"

  Scenario: A receipt the worker never consumed is recovered by the boot-time sweep
    Given an independent app has an unconsumed verified receipt
    When the worker journals a validly-signed completion callback and dies before consuming it
    Then the journal holds one pending receipt
    And the retainer workflow is still at "sent_for_signature__pending"
    When the boot-time replay sweep drains the journal
    Then the sweep drains without error
    And the retainer workflow has advanced to "END"
    And the notation row state is "END"
    And the signature row state is "completed"
    And the journal has no pending receipts
    And the independent app still has its pending receipt

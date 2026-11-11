Feature: Declared signers drive a non-retainer envelope

  The employee offer letter is not a retainer: its template declares its
  own signers in frontmatter — the company's authorized officer embedded,
  the employee emailed — and the generalized send flow builds the
  manifest from those declarations instead of the retainer's default
  [client, firm] pair.

  Background:
    Given an offer-letter matter opened for "Libra" <libra@example.com>

  Scenario: The template's declared signers route the envelope
    When the client answers the company question:
      | value |
      | Aurora Market LLC |
    And the lawyer walks the rest of the offer questionnaire:
      | value |
      | Rex Vice; President; 1 Main St; Las Vegas; NV; 89101; USA |
      | Libra Prime; Barista; 2 Worker Way; Reno; NV; 89501; USA |
      | libra.prime@example.com |
      | Nevada |
      | 2026-12-01 |
      | Store manager; manages the Reno store; Aurora Market LLC; starting 2026-12-15. |
      | Exempt under the executive duties test at the stated salary. |
      | 62400 |
      | Salaried, exempt |
      | Every other Friday |
      | Biweekly |
      | None proposed. |
      | State statutory benefits and leave only. |
      | Contingent on work authorization and a lawful background check. |
      | contacts:Rex Vice;rex@aurora.example |
    And the attorney approves and sends the document
    Then the matter has a signature request
    And the envelope routes the company before the employee
    And the employee recipient is emailed at the bound address

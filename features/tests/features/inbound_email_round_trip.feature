Feature: Inbound-email round trip, end to end

  The "headless Front" loop: a client emails support, the firm threads the
  message into a conversation, and an attorney binds the thread to the client's
  matter with an `@link` command. Binding and approval stay inside the firm.
  A Lawyer reply does not send. This follows Aries, who has more than one
  open matter — so the engine can't auto-route the thread and the attorney
  picks the matter with `@link`.

  Background:
    Given a client named "Aries" <aries@example.com> with two open matters
    And a lawyer "lawyer@neonlaw.com"

  Scenario: A client email threads, binds to the matter, and the attorney does not send
    When the client emails support asking about their filing
    Then a support conversation is opened for the client
    And the firm is notified with a reply-to thread token
    When the attorney replies "@link" to the thread and answers the client
    Then the conversation is bound to the client's matter
    And the attorney's answer is not sent to the client

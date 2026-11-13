Feature: A completed signature files itself back to the matter, case number or not (LAW-344)
  A leading paper can be signed before the court assigns a case number:
  matter_caption.case_number is optional metadata and signing never reads
  it. What completion adds is the file-back — the verified webhook files
  the signed PDF and the Certificate of Completion on the Project's
  document set exactly once, so a Connect redelivery adds nothing, while
  a forged callback files nothing. The loop runs against the Benton
  County declaration template — the generalization proof that the same
  seam serves any signed paper, not just the retainer.

  Background:
    Given a Neon Law Navigator app with an HMAC-secured e-signature webhook
    And a Benton County declaration parked at sent_for_signature__pending on a matter with no case number

  Scenario: A verified completion files the signed copy and its certificate on the matter
    When the provider posts a validly-signed completion callback
    Then the response status is 200
    And the signed declaration is filed on the matter exactly once
    And its certificate of completion is filed on the matter exactly once
    And the filed signed copy records that the client signed by DocuSign

  Scenario: A redelivered completion files nothing more
    When the provider posts a validly-signed completion callback
    Then the response status is 200
    When the provider posts the same completion callback again
    Then the response status is 200
    And the signed declaration is filed on the matter exactly once
    And its certificate of completion is filed on the matter exactly once

  Scenario: A forged completion files nothing
    When an attacker posts a completion callback with a forged signature
    Then the response status is 401
    And the matter has no filed documents

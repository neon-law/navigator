Feature: Bundled-template workflow composition (nonprofit)

  The firm's nonprofit practice covers 501(c)(3) formation, the annual Form
  990, and state-level charitable solicitation registration. These scenarios
  pin each template's reusable-step composition — like
  `legal_workflow_shapes.feature` does for the rest of the firm's practice —
  so an accidental reshape on the nonprofit templates surfaces as a named
  failing scenario.

  Each omitted-END scenario verifies that author normalization preserves
  the full workflow, while typed runtime validation still requires an
  explicit terminal END.

  Scenario: Nevada 501(c)(3) formation questionnaire walks mission → board → agent → END
    Given the bundled template "notations/forms/united_states/nevada/state/nv__nonprofit_501c3_formation.md"
    Then the questionnaire transitions, in BEGIN-first order, are:
      | from                           | to                             |
      | BEGIN                          | custom_text__mission_statement |
      | custom_text__mission_statement | people__board_members         |
      | people__board_members          | person__registered_agent      |
      | person__registered_agent       | END                           |

  Scenario: Nevada 501(c)(3) formation workflow signs, reviews, and mails the articles
    Given the bundled template "notations/forms/united_states/nevada/state/nv__nonprofit_501c3_formation.md"
    Then the workflow transitions, in BEGIN-first order, are:
      | from             | to               |
      | BEGIN            | board_signatures |
      | board_signatures | lawyer_review     |
      | lawyer_review     | mailroom_send    |
      | mailroom_send    | END              |

  Scenario: Nevada 501(c)(3) formation template with omitted END keeps its workflow and strict runtime guard
    Given the bundled template "notations/forms/united_states/nevada/state/nv__nonprofit_501c3_formation.md" with the workflow END declaration removed
    Then the authored workflow preserves every transition and supplies a terminal END
    And the typed workflow still rejects a missing END declaration

  Scenario: Form 990 questionnaire walks tax_year → revenue → END
    Given the bundled template "notations/forms/united_states/federal/irs/us__form_990.md"
    Then the questionnaire transitions, in BEGIN-first order, are:
      | from                          | to                            |
      | BEGIN                         | custom_datetime__tax_year            |
      | custom_datetime__tax_year            | custom_text__revenue_strategy |
      | custom_text__revenue_strategy | END                           |

  Scenario: Form 990 workflow signs, reviews, and mails to the IRS
    Given the bundled template "notations/forms/united_states/federal/irs/us__form_990.md"
    Then the workflow transitions, in BEGIN-first order, are:
      | from             | to               |
      | BEGIN            | board_signatures |
      | board_signatures | lawyer_review     |
      | lawyer_review     | mailroom_send    |
      | mailroom_send    | END              |

  Scenario: Form 990 template with omitted END keeps its workflow and strict runtime guard
    Given the bundled template "notations/forms/united_states/federal/irs/us__form_990.md" with the workflow END declaration removed
    Then the authored workflow preserves every transition and supplies a terminal END
    And the typed workflow still rejects a missing END declaration

  Scenario: Charitable solicitation registration questionnaire walks period → activities → END
    Given the bundled template "notations/forms/united_states/nevada/state/nv__charitable_solicitation_registration.md"
    Then the questionnaire transitions, in BEGIN-first order, are:
      | from                                | to                                  |
      | BEGIN                                   | custom_single_choice__annual_or_amended |
      | custom_single_choice__annual_or_amended | custom_text__fundraising_activities |
      | custom_text__fundraising_activities     | END                                 |

  Scenario: Charitable solicitation registration workflow reviews and mails the statement
    Given the bundled template "notations/forms/united_states/nevada/state/nv__charitable_solicitation_registration.md"
    Then the workflow transitions, in BEGIN-first order, are:
      | from          | to            |
      | BEGIN         | lawyer_review  |
      | lawyer_review  | mailroom_send |
      | mailroom_send | END           |

  Scenario: Charitable solicitation template with omitted END keeps its workflow and strict runtime guard
    Given the bundled template "notations/forms/united_states/nevada/state/nv__charitable_solicitation_registration.md" with the workflow END declaration removed
    Then the authored workflow preserves every transition and supplies a terminal END
    And the typed workflow still rejects a missing END declaration

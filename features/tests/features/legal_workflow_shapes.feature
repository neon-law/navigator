Feature: Bundled-template questionnaire composition

  The engagement letter is BDD-tested end-to-end via the walker; the other
  bundled templates lock down their questionnaire composition here. The
  `workflow_integrity` workspace test owns generic engine invariants (BEGIN
  present, END reachable, every transition target exists, every workflow
  prefix resolves to a `StepKind`); these scenarios pin the catalog so an
  accidental reshape of a remaining template surfaces as a named failing
  scenario.

  Each template also verifies the author/runtime boundary: omitting `END:`
  preserves the entire authored workflow through normalization, while typed
  runtime validation still rejects a spec without its explicit terminal END.

  Scenario: Engagement letter questionnaire walks entity → office → client → project → terms → END
    Given the bundled template "notations/neon_law/onboarding__letter.md"
    Then the questionnaire transitions, in BEGIN-first order, are:
      | from                                    | to                                      |
      | BEGIN                                   | entity                                  |
      | entity                                  | address__principal_office               |
      | address__principal_office               | person__client                          |
      | person__client                          | project__engagement                     |
      | project__engagement                     | custom_datetime__engagement_start_date  |
      | custom_datetime__engagement_start_date  | custom_text__engagement_scope           |
      | custom_text__engagement_scope           | custom_single_choice__governing_law     |
      | custom_single_choice__governing_law     | END                                     |

  Scenario: Engagement letter template with omitted END keeps its workflow and strict runtime guard
    Given the bundled template "notations/neon_law/onboarding__letter.md" with the workflow END declaration removed
    Then the authored workflow preserves every transition and supplies a terminal END
    And the typed workflow still rejects a missing END declaration

  Scenario: Closing letter questionnaire walks entity → client → project → END
    Given the bundled template "notations/neon_law/offboarding__letter.md"
    Then the questionnaire transitions, in BEGIN-first order, are:
      | from                               | to                                 |
      | BEGIN                              | entity                             |
      | entity                             | person__client                     |
      | person__client                     | project                            |
      | project                            | END                                |

  Scenario: Closing letter template with omitted END keeps its workflow and strict runtime guard
    Given the bundled template "notations/neon_law/offboarding__letter.md" with the workflow END declaration removed
    Then the authored workflow preserves every transition and supplies a terminal END
    And the typed workflow still rejects a missing END declaration

  Scenario: Nevada LLC formation questionnaire walks client → company → agent → management → members → date → END
    Given the bundled template "notations/forms/united_states/nevada/state/nv__llc_formation.md"
    Then the questionnaire transitions, in BEGIN-first order, are:
      | from                                         | to                                           |
      | BEGIN                                        | person__client                               |
      | person__client                               | entity__company                              |
      | entity__company                              | person__registered_agent                     |
      | person__registered_agent                     | custom_single_choice__management_structure   |
      | custom_single_choice__management_structure   | people__managing_members                     |
      | people__managing_members                     | custom_datetime__formation_date              |
      | custom_datetime__formation_date              | END                                          |

  Scenario: Nevada LLC formation template with omitted END keeps its workflow and strict runtime guard
    Given the bundled template "notations/forms/united_states/nevada/state/nv__llc_formation.md" with the workflow END declaration removed
    Then the authored workflow preserves every transition and supplies a terminal END
    And the typed workflow still rejects a missing END declaration

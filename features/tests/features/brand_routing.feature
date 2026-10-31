Feature: Public routing on the firm's own host

  The `neon` binary composes the public routes for every house brand this
  repository registers, resolved per request from the `Host:` header; the
  firm holds the root on its own registered hosts. A path with no firm page
  answers `404`, the same answer as a path that never existed.

  This harness drives every scenario against the firm's own host, so
  `og:site_name` is "Neon Law" throughout — that name is what these scenarios
  assert; its absence marks a page mounted under the wrong brand. The
  exhaustive host-by-brand matrix (a registered host answering its own
  brand, an unregistered host redirecting) lives in `server/tests/routes.rs`,
  which drives one composed router directly rather than building a whole app
  per scenario.

  Background:
    Given the Neon Law Navigator public site is running

  # Every scenario here builds a whole app in its Background, and each build
  # takes a slice of the Dioxus pinned-worker pool, so this file stays a thin
  # representative sample on purpose. The exhaustive per-path table lives in
  # `server/tests/routes.rs`, which drives one router.
  #
  # This harness loads no Catalog content, so the material catalogs are not
  # asserted here; `server/tests/firm_routes.rs` covers them against real
  # content.

  Scenario: The firm's front door is the site root
    When a visitor opens /
    Then the response status is 200
    And the page is branded "Neon Law"

  Scenario Outline: A launched practice host serves its authored home page
    When a visitor opens / on host "<host>"
    Then the response status is 200
    And the page is branded "<brand>"
    And the response body contains "<title>"
    And the response body contains "<copy>"
    And the response body does not contain "Coming Soon"

    Examples:
      | host                           | brand                    | title                                      | copy                                  |
      | staging.neonlaw.com            | Neon Law                 | Neon Law \| Home                           | Keep building.                         |
      | staging.deleteyourdata.com     | DeleteYourData.com       | DeleteYourData.com \| Home                  | Your Life. Less Exposed.               |
      | staging.vestaestateplanning.com | Vesta Estate Planning    | Vesta Estate Planning \| Home    | Your life is part of a larger story.   |
      | staging.misericordialaw.com     | Misericordia Injury Law  | Misericordia Injury Law \| Home  | You were hurt. We are here to listen.  |
      | staging.abhayaimmigration.com   | Abhaya Immigration       | Abhaya Immigration \| Home       | Your next chapter, without fear.       |
      | staging.deleteyourdebt.com      | DeleteYourDebt.com       | DeleteYourDebt.com \| Home        | A next step beyond debt.                |

  Scenario Outline: A live holding host serves its explicit holding page
    When a visitor opens / on host "<host>"
    Then the response status is 200
    And the page is branded "<brand>"
    And the response body contains "<title>"
    And the response body contains "<marker>"

    Examples:
      | host                       | brand           | title                    | marker                                  |
      | staging.summonsdefense.nyc | Summons Defense | Summons Defense \| Home  | A city summons. A clear next step.      |
      | staging.guiebandshook.com  | Guieb & Shook   | Guieb & Shook PC \| Home | Attorneys at Law.                       |

  Scenario Outline: A Cyber Injury Law practice page serves on its own host
    When a visitor opens <path> on host "<host>"
    Then the response status is 200
    And the page is branded "CyberInjuryLaw"
    And the response body contains "<og-title>"

    Examples:
      | host                       | path           | og-title                            |
      | staging.cyberinjurylaw.com | /car-accidents | CyberInjuryLaw \| Car accident lawyers |
      | staging.cyberinjurylaw.com | /cybercrime    | CyberInjuryLaw \| Cybercrime lawyers   |
      | staging.cyberinjurylaw.com | /theft         | CyberInjuryLaw \| Theft claims      |
      | staging.cyberinjurylaw.com | /data-theft    | CyberInjuryLaw \| Data theft lawyers  |

  Scenario Outline: The firm's published anonymous surface serves at the root
    # Each is anonymous: a stranger deciding whether to hire a lawyer must not
    # meet a login door.
    When a visitor opens <path>
    Then the response status is 200

    Examples:
      | path       |
      | /notations |
      | /contact   |
      | /services  |

  Scenario Outline: A retired practice path answers 404 rather than redirecting
    # `/business` and `/disputes` no longer name a page. They answer the same
    # `404` as a path that never existed, and carry no `Location`: a reader
    # who follows an old link is told the page is gone rather than silently
    # rerouted to the root, where they would have to find the section
    # themselves. `/services` is not in this list: it answers the firm's fee
    # schedule again.
    When a visitor opens <path>
    Then the response status is 404
    And the response carries no redirect

    Examples:
      | path      |
      | /disputes |
      | /business |

  Scenario Outline: A path with no firm page answers 404
    When a visitor opens <path>
    Then the response status is 404
    And the response carries no redirect

    Examples:
      | path                     |
      | /foundation              |
      | /foundation/mission      |
      | /foundation/transparency |
      | /mission                 |
      | /transparency            |
      | /education               |
      | /does-not-exist          |

  # The footer is the three-line legal strip: the firm-resolved copyright,
  # the compiled trademark notice when the brand carries one, and the
  # brand's own disclaimer. Everything else the old footer rendered —
  # offices, bar licenses, associations, the "Our Family" row, social
  # profiles, the contact band — is gone from every public page. With no
  # Firm rows in the store, the copyright resolves the compiled fallback
  # entity.
  Scenario Outline: A public page's footer is the three-line legal strip
    When a visitor opens / on host "<host>"
    Then the response status is 200
    And the footer copyright names "<entity>"
    And the footer renders the trademark notice: <trademark>
    And the footer renders the attorney-advertising disclaimer
    And the footer carries no firm detail beyond the legal strip

    Examples:
      | host                             | entity           | trademark |
      | staging.neonlaw.com              | Guieb & Shook PC | yes       |
      | staging.deleteyourdata.com       | Guieb & Shook PC | no        |
      | staging.vestaestateplanning.com  | Guieb & Shook PC | no        |

  # The live-resolution story, end to end: an Owner moves a brand to a
  # firm whose entity no compiled fallback names, and the brand's public
  # pages resolve the new firm's entity on the next request. The compiled
  # trademark notice stays — the registration belongs to the mark, not to
  # whichever firm wears the brand. This scenario runs last: the move
  # persists in the shared store, so the Neon host's footer names the new
  # entity from here on.
  Scenario: A moved brand's footer follows its new firm
    Given the brand "neon" is moved to a firm whose entity is "Mobility Test Holdings PC"
    When a visitor opens / on host "staging.neonlaw.com"
    Then the response status is 200
    And the footer copyright names "Mobility Test Holdings PC"
    And the footer renders the trademark notice: yes
    And the footer renders the attorney-advertising disclaimer
    And the footer carries no firm detail beyond the legal strip

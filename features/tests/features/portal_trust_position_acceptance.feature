Feature: A client sees their allocated synthetic trust position

  The shared ENG-821 scenario applies the portfolio twice in this isolated
  runner, then checks the client's own allocation and withholds the pooled
  account total and provider account identifier.

  Background:
    Given the Neon Law Navigator app is running

  Scenario: The client sees their own pooled draw allocation
    Given the versioned synthetic portfolio is applied twice
    When shared acceptance scenario "client.trust-allocation" is requested
    Then shared acceptance scenario "client.trust-allocation" passes

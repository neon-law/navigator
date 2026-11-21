"""Check the release browser report and streamed output after Nextest succeeds."""

import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path


def check(report, log):
    expected = {"server::browser_e2e", "server::accessibility_e2e", "server::notation_preview_layout"}
    suites = ET.parse(report).getroot().findall("testsuite")
    if {suite.get("name") for suite in suites} != expected or len(suites) != len(expected):
        raise ValueError("expected one JUnit suite for each required browser binary")
    for suite in suites:
        cases = suite.findall("testcase")
        if not cases or len(cases) != int(suite.attrib["tests"]):
            raise ValueError(f"{suite.get('name')} has an empty or incomplete JUnit report")
        if any(case.find(tag) is not None for case in cases for tag in ("skipped", "failure", "error")):
            raise ValueError(f"{suite.get('name')} has a skipped or unsuccessful test")
    with Path(log).open() as lines:
        for line in lines:
            if re.search(r"^\s*skipping\b.*\btest:", line, re.IGNORECASE):
                raise ValueError(f"browser test returned early: {line.strip()}")


if __name__ == "__main__":
    try:
        check(*sys.argv[1:])
    except (OSError, ValueError, ET.ParseError) as error:
        sys.exit(str(error))

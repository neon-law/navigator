import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import repair
import runner


class MaintenanceTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name) / "repo"
        self.repo.mkdir()
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True)
        runner.run(
            ["git", "remote", "add", "origin", "https://github.com/example/synthetic"],
            self.repo,
        )

    def test_cli_updates_the_pin_and_preserves_project_fields(self):
        manifest = self.repo / "navigator.yaml"
        manifest.write_text("version: '26.1.8' # pinned\nproject:\n  name: synthetic\n")
        workflow = self.repo / ".github/workflows/ci.yml"
        workflow.parent.mkdir(parents=True)
        workflow.write_text(
            "uses: neon-law/navigator/.github/workflows/project-gate.yml@26.1.8\n"
        )
        runner.update_pins(self.repo, "26.1.8", "26.1.9")
        self.assertEqual(
            manifest.read_text(),
            "version: '26.1.9' # pinned\nproject:\n  name: synthetic\n",
        )
        self.assertTrue(workflow.read_text().endswith("@26.1.9\n"))

    def test_debug_queue_can_apply_agent_authored_code_only_within_policy(self):
        code = self.repo / "src/example.ts"
        code.parent.mkdir()
        code.write_text("export const option = 'legacy';\n")
        runner.run(["git", "add", "src/example.ts"], self.repo)
        policy = Path(self.tmp.name) / "policy.json"
        policy.write_text(
            json.dumps({"https://github.com/example/synthetic": ["src/example.ts"]})
        )
        work = Path(self.tmp.name)
        (work / "release-notes").write_text("Use the current option in examples.")
        with patch.dict(
            os.environ, {"NAVIGATOR_MAINTENANCE_REPAIR_SOURCES": str(policy)}
        ):
            with patch.object(
                repair,
                "vertex_request",
                return_value={
                    "edits": [
                        {
                            "file": "src/example.ts",
                            "before": "'legacy'",
                            "after": "'current'",
                        }
                    ]
                },
            ):
                repair.repair_sources(self.repo, work, "26.1.9", ["Y001"])
            self.assertIn("'current'", code.read_text())
            with (
                patch.object(
                    repair,
                    "vertex_request",
                    return_value={
                        "edits": [{"file": "../outside", "before": "x", "after": "y"}]
                    },
                ),
                self.assertRaises(runner.Blocked),
            ):
                repair.repair_sources(self.repo, work, "26.1.9", [])
        self.assertFalse((work / "outside").exists())

    def test_external_symlink_cannot_enter_maintenance(self):
        (self.repo / "navigator.yaml").symlink_to(Path(self.tmp.name) / "outside")
        runner.run(["git", "add", "navigator.yaml"], self.repo)
        with self.assertRaises(runner.Blocked):
            runner.safe_tree(self.repo)

    def test_unpublished_or_candidate_tag_is_refused(self):
        for tag in ("latest", "26.1.9-rc.1", "26.1.9/other"):
            with self.assertRaises(runner.Blocked):
                runner.release_date(tag)

    def test_only_deployment_for_the_reviewed_merge_can_complete_delivery(self):
        head = "a" * 40
        report = {
            "merge_commit": head,
            "deployment": {"commit": head, "conclusion": "success"},
        }
        self.assertEqual(
            runner.delivery_commit("progress\n" + json.dumps(report)), head
        )
        for deployment in [
            None,
            {"commit": "b" * 40, "conclusion": "success"},
            {"commit": head, "conclusion": "failure"},
        ]:
            with self.assertRaises(runner.Blocked):
                runner.delivery_commit(json.dumps({**report, "deployment": deployment}))

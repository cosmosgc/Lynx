import tempfile
import unittest
from pathlib import Path

from _backlog_dashboard.app import (
    backlog_categories,
    backlog_summary,
    create_app,
    load_backlog,
    load_phase_progress,
    order_tasks,
)


class BacklogDashboardTests(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        self.todo = self.root / "docs" / "TODO"
        self.todo.mkdir(parents=True)
        relevant_file = self.root / "crates" / "engine_sim" / "src" / "nav.rs"
        relevant_file.parent.mkdir(parents=True)
        relevant_file.write_text("// sample", encoding="utf-8")
        (self.todo / "10-sample.md").write_text(
            """# Sample

## SIM-001 — Movement

- **Priority:** P0
- **Status:** In progress — basic slice
- **Depends on:** `D5`, —
- **References:** `SIM-002`, [guide](../guide.md)

**Why:** A server-authoritative movement loop is required.

**Scope**

- Update navigation in `crates/engine_sim/src/nav.rs`.
- Keep validation deterministic.

**Acceptance criteria**

- [x] Deterministic output.
- [ ] Network coverage.

## SIM-002 — Transport

- **Priority:** P1
- **Status:** Blocked — movement first
- **Depends on:** `SIM-001`
- **References:** `SIM-001`

**Acceptance criteria**

- [ ] Deliver frames.

## SIM-003 — Ready task

- **Priority:** P2
- **Status:** Ready
- **Depends on:** —
- **References:** —
""",
            encoding="utf-8",
        )
        (self.root / "README.md").write_text(
            """Phase 0  Architecture proof          [██████████████████] 100%  Complete
Phase 1  Engine skeleton             [███████████████░░░]  85%  In progress — client pending
""",
            encoding="utf-8",
        )

    def tearDown(self):
        self.temp_dir.cleanup()

    def test_parses_metadata_dependencies_and_acceptance(self):
        items = load_backlog(self.todo)
        first, second, third = items

        self.assertEqual(first["status"], "in_progress")
        self.assertEqual(first["status_detail"], "basic slice")
        self.assertEqual(first["dependency_ids"], [])
        self.assertEqual(first["external_dependencies"], ["D5"])
        self.assertEqual(first["reference_ids"], ["SIM-002"])
        self.assertIn("A server-authoritative movement loop", first["summary"])
        self.assertEqual(
            first["scope"],
            [
                "Update navigation in `crates/engine_sim/src/nav.rs`.",
                "Keep validation deterministic.",
            ],
        )
        self.assertEqual(first["relevant_files"], ["crates/engine_sim/src/nav.rs"])
        self.assertEqual(first["acceptance"][0]["complete"], True)
        self.assertEqual(len(first["acceptance"]), 2)
        self.assertNotIn("Foundation", first["acceptance"][-1]["text"])
        self.assertEqual(second["dependency_ids"], ["SIM-001"])
        self.assertEqual(first["dependent_ids"], ["SIM-002"])
        self.assertEqual(third["status"], "ready")

    def test_orders_prerequisites_before_dependents(self):
        items = load_backlog(self.todo)
        ordered_ids = [item["id"] for item in order_tasks(items)]

        self.assertLess(ordered_ids.index("SIM-001"), ordered_ids.index("SIM-002"))
        self.assertEqual(set(ordered_ids), {"SIM-001", "SIM-002", "SIM-003"})

    def test_summary_counts_known_statuses_and_completion(self):
        summary = backlog_summary(load_backlog(self.todo))

        self.assertEqual(summary["total"], 3)
        self.assertEqual(summary["done"], 0)
        self.assertEqual(summary["in_progress"], 1)
        self.assertEqual(summary["blocked"], 1)
        self.assertEqual(summary["ready"], 1)
        self.assertEqual(summary["completion_percent"], 0)
        self.assertEqual(
            {segment["status"]: segment["count"] for segment in summary["status_segments"]},
            {"in_progress": 1, "blocked": 1, "ready": 1},
        )
        category = backlog_categories(load_backlog(self.todo))[0]
        self.assertEqual(category["total"], 3)
        self.assertEqual(category["ready"], 1)
        self.assertEqual(category["completion_percent"], 0)
        self.assertAlmostEqual(
            sum(segment["percent"] for segment in category["status_segments"]),
            100,
        )

    def test_reads_readme_phase_progress_separately(self):
        phases = load_phase_progress(self.root / "README.md")

        self.assertEqual([phase["percent"] for phase in phases], [100, 85])
        self.assertEqual(phases[0]["status"], "Complete")

    def test_home_and_api_routes(self):
        client = create_app(self.todo).test_client()

        page = client.get("/")
        self.assertEqual(page.status_code, 200)
        self.assertIn(b"Ready means the task can be started", page.data)
        self.assertIn(b"Phase 0", page.data)
        self.assertIn(b"Task status distribution", page.data)
        self.assertIn(b"progress-segment status-ready", page.data)
        response = client.get("/api/items")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json["summary"]["total"], 3)
        self.assertEqual(response.json["summary"]["ready"], 1)
        self.assertEqual(len(response.json["categories"]), 1)
        self.assertEqual(response.json["phases"][1]["percent"], 85)
        self.assertEqual(client.get("/source?file=docs/TODO/10-sample.md").status_code, 200)
        self.assertEqual(client.get("/source?file=../../outside.md").status_code, 404)

    def test_full_dependency_graph_view_includes_all_tasks_and_links(self):
        response = create_app(self.todo).test_client().get("/dependencies")

        self.assertEqual(response.status_code, 200)
        self.assertIn(b"Full dependency graph", response.data)
        self.assertIn(b"SIM-001", response.data)
        self.assertIn(b"calculateRanks", response.data)
        self.assertIn(b'fill="${statusFills[item.status]}"', response.data)
        self.assertIn(b'stroke="${statusColors[item.status]}"', response.data)
        self.assertIn(b"Arrows point from prerequisite to dependent task", response.data)

    def test_task_order_view_includes_prompt_builder(self):
        response = create_app(self.todo).test_client().get("/task-order")

        self.assertEqual(response.status_code, 200)
        self.assertIn(b"Add prerequisites", response.data)
        self.assertIn(b"Copy LLM prompt", response.data)
        self.assertIn(b"function makePrompt", response.data)
        self.assertIn(b"opt-delegate", response.data)
        self.assertIn(b"opt-parallel", response.data)
        self.assertIn(b"commit-policy", response.data)
        self.assertIn(b"per-task", response.data)
        self.assertIn(b"crates/engine_sim/src/nav.rs", response.data)


if __name__ == "__main__":
    unittest.main()

import tempfile
import sqlite3
import unittest
from pathlib import Path
from unittest.mock import patch

import _backlog_dashboard.app as dashboard_app
from _backlog_dashboard.app import (
    backlog_categories,
    backlog_summary,
    create_app,
    load_backlog,
    load_decisions,
    load_phase_progress,
    order_tasks,
    scan_user_todos,
    sync_user_todos,
    suggest_next_tasks,
)


class BacklogDashboardTests(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        self.todo = self.root / "docs" / "TODO"
        self.todo.mkdir(parents=True)
        relevant_file = self.root / "crates" / "engine_sim" / "src" / "nav.rs"
        relevant_file.parent.mkdir(parents=True)
        relevant_file.write_text("// TODO: add a bounded preview\n// sample", encoding="utf-8")
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
        self.state_db = self.root / "state.sqlite3"

    def make_app(self):
        return create_app(self.todo, self.state_db)

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
        app = self.make_app()
        self.assertTrue(app.config["TEMPLATES_AUTO_RELOAD"])
        client = app.test_client()

        page = client.get("/")
        self.assertEqual(page.status_code, 200)
        self.assertIn(b"Ready means the task can be started", page.data)
        self.assertIn(b"Backlog navigation", page.data)
        self.assertIn(b"Source TODOs", page.data)
        self.assertNotIn(b"Roadmap phases", page.data)
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

    def test_decisions_view_connects_roadmap_gates_adrs_and_tasks(self):
        adr_dir = self.root / "docs" / "adr"
        adr_dir.mkdir(parents=True)
        (adr_dir / "0022-combat.md").write_text(
            "# ADR 0022: Combat model\n\n- **Status:** Proposed\n\n"
            "## Goal\n\nA proposed combat decision summary.\n",
            encoding="utf-8",
        )
        roadmap = self.todo / "00-roadmap.md"
        roadmap.write_text(
            "# Roadmap\n\n## Decision gates\n\n"
            "| Gate | Question | Owner | Unblocks |\n"
            "| --- | --- | --- | --- |\n"
            "| **D5 — Editor scope** | Should ADR 0022 be accepted? | Content + client | `SIM-001` |\n",
            encoding="utf-8",
        )
        sample = self.todo / "10-sample.md"
        sample.write_text(
            sample.read_text(encoding="utf-8").replace(
                "- **References:** `SIM-002`, [guide](../guide.md)",
                "- **References:** `SIM-002`, ADR 0022, [guide](../guide.md)",
            ),
            encoding="utf-8",
        )

        client = self.make_app().test_client()
        response = client.get("/decisions")
        self.assertEqual(response.status_code, 200)
        self.assertIn(b"Architecture decisions", response.data)
        self.assertIn(b"D5 ", response.data)
        self.assertIn(b"ADR 0022", response.data)
        self.assertIn(b"SIM-001", response.data)
        self.assertIn(b"/source?file=docs/adr/0022-combat.md#L1", response.data)

        api = client.get("/api/decisions")
        self.assertEqual(api.status_code, 200)
        self.assertEqual(api.json["gates"][0]["related_task_ids"], ["SIM-001"])
        self.assertEqual(api.json["gates"][0]["adr_ids"], ["ADR 0022"])
        self.assertEqual(api.json["adrs"][0]["gate_ids"], ["D5"])
        self.assertEqual(api.json["adrs"][0]["related_task_ids"], ["SIM-001"])
        task = next(
            item
            for item in client.get("/api/items").json["items"]
            if item["id"] == "SIM-001"
        )
        self.assertEqual(task["decision_gate_ids"], ["D5"])
        self.assertEqual(task["adr_ids"], ["ADR 0022"])
        self.assertIn(b"Connected decisions", client.get("/").data)
        self.assertIn(b"/decisions#${id.toLowerCase()}", client.get("/").data)
        self.assertIn(b"Connected decisions", client.get("/dependencies").data)

    def test_source_todo_inbox_and_manager_report(self):
        markers = scan_user_todos(self.root)
        self.assertEqual(markers[0]["kind"], "TODO")
        self.assertEqual(markers[0]["file"], "crates/engine_sim/src/nav.rs")
        self.assertEqual(markers[0]["line"], 1)

        client = self.make_app().test_client()
        todos = client.get("/todos")
        self.assertEqual(todos.status_code, 200)
        self.assertIn(b"Source TODO inbox", todos.data)
        self.assertIn(b"add a bounded preview", todos.data)
        self.assertEqual(client.get("/source?file=crates/engine_sim/src/nav.rs").status_code, 200)
        report = client.get("/reports")
        self.assertEqual(report.status_code, 200)
        self.assertIn(b"Manager report", report.data)
        self.assertIn(b"Recommended next", report.data)
        api = client.get("/api/report")
        self.assertEqual(api.status_code, 200)
        self.assertEqual(len(api.json["user_todos"]), 1)

    def test_source_todo_cache_rereads_only_changed_files_and_removes_deleted_files(self):
        source = self.root / "crates" / "engine_sim" / "src" / "nav.rs"
        self.make_app()
        with patch(
            "_backlog_dashboard.app._markers_from_source",
            wraps=dashboard_app._markers_from_source,
        ) as parse:
            initial = sync_user_todos(self.root, self.state_db)
            self.assertEqual(len(initial), 1)
            initial_parse_count = parse.call_count
            self.assertGreater(initial_parse_count, 0)
            self.assertEqual(len(sync_user_todos(self.root, self.state_db)), 1)
            self.assertEqual(parse.call_count, initial_parse_count)

            source.write_text("# FIXME: replacement marker\n", encoding="utf-8")
            changed = sync_user_todos(self.root, self.state_db)
            self.assertEqual(parse.call_count, initial_parse_count + 1)
            self.assertEqual(changed[0]["kind"], "FIXME")

            source.unlink()
            self.assertEqual(sync_user_todos(self.root, self.state_db), [])

    def test_full_dependency_graph_view_includes_all_tasks_and_links(self):
        response = self.make_app().test_client().get("/dependencies")

        self.assertEqual(response.status_code, 200)
        self.assertIn(b"Full dependency graph", response.data)
        self.assertIn(b"SIM-001", response.data)
        self.assertIn(b"calculateRanks", response.data)
        self.assertIn(b'fill="${statusFills[item.status]}"', response.data)
        self.assertIn(b'stroke="${statusColors[item.status]}"', response.data)
        self.assertIn(b"Arrows point from prerequisite to dependent task", response.data)

    def test_task_order_view_includes_prompt_builder(self):
        response = self.make_app().test_client().get("/task-order")

        self.assertEqual(response.status_code, 200)
        self.assertIn(b"Add prerequisites", response.data)
        self.assertIn(b"Copy LLM prompt", response.data)
        self.assertIn(b"function makePrompt", response.data)
        self.assertIn(b"opt-delegate", response.data)
        self.assertIn(b"opt-parallel", response.data)
        self.assertIn(b"commit-policy", response.data)
        self.assertIn(b"per-task", response.data)
        self.assertIn(b"crates/engine_sim/src/nav.rs", response.data)
        self.assertIn(b"Status is read from the backlog Markdown", response.data)
        self.assertIn(b"populateToggles", response.data)
        self.assertNotIn(b'<select id="category"', response.data)
        self.assertNotIn(b"data-status-for", response.data)
        self.assertNotIn(b"Change status for", response.data)
        self.assertIn(b'href="/history">History</a>', response.data)

    def test_task_order_filter_hiding_overrides_task_grid_display(self):
        response = self.make_app().test_client().get("/task-order")

        self.assertEqual(response.status_code, 200)
        self.assertIn(b".task[hidden] { display:none; }", response.data)
        self.assertIn(b"row.hidden = !visible", response.data)

    def test_suggestions_skip_blocked_tasks_and_do_not_assume_prior_suggestions_done(self):
        source = self.todo / "10-sample.md"
        markdown = source.read_text(encoding="utf-8").replace(
            "- **Depends on:** `D5`, —",
            "- **Depends on:** —",
        )
        source.write_text(markdown, encoding="utf-8")
        suggestions = suggest_next_tasks(order_tasks(load_backlog(self.todo)), 2)

        self.assertEqual([task["id"] for task in suggestions], ["SIM-001", "SIM-003"])
        self.assertEqual(suggestions[0]["suggestion_score"], 55)
        self.assertIn("continue in-progress work (+10)", suggestions[0]["suggestion_reason"])
        self.assertIn("unlocks 1 dependent task(s) (+5)", suggestions[0]["suggestion_reason"])
        self.assertEqual(suggestions[1]["id"], "SIM-003")

    def test_in_progress_work_with_unmet_dependency_is_penalized(self):
        source = self.todo / "10-sample.md"
        markdown = source.read_text(encoding="utf-8").replace(
            "- **Depends on:** `D5`, —",
            "- **Depends on:** —",
        ).replace(
            "- **Status:** Blocked — movement first",
            "- **Status:** In progress — transport foundation",
        )
        source.write_text(markdown, encoding="utf-8")

        suggestions = suggest_next_tasks(order_tasks(load_backlog(self.todo)), 3)

        transport = next(task for task in suggestions if task["id"] == "SIM-002")
        self.assertEqual(transport["suggestion_score"], 30)
        self.assertIn("1 unresolved task dependency(s) (-10)", transport["suggestion_reason"])

    def test_ready_task_with_unmet_dependency_is_not_suggested(self):
        source = self.todo / "10-sample.md"
        markdown = source.read_text(encoding="utf-8").replace(
            "- **Depends on:** `D5`, —",
            "- **Depends on:** —",
        ).replace(
            "- **Status:** Blocked — movement first",
            "- **Status:** Ready",
        )
        source.write_text(markdown, encoding="utf-8")

        suggestions = suggest_next_tasks(order_tasks(load_backlog(self.todo)), 3)

        self.assertNotIn("SIM-002", [task["id"] for task in suggestions])

    def test_blocked_task_is_not_suggested_even_when_dependencies_are_done(self):
        source = self.todo / "10-sample.md"
        markdown = source.read_text(encoding="utf-8").replace(
            "- **Depends on:** `D5`, —",
            "- **Depends on:** —",
        ).replace(
            "- **Status:** In progress — basic slice",
            "- **Status:** Done",
        )
        source.write_text(markdown, encoding="utf-8")

        suggestions = suggest_next_tasks(order_tasks(load_backlog(self.todo)), 3)

        self.assertNotIn("SIM-002", [task["id"] for task in suggestions])

    def test_task_order_renders_suggestion_and_rejects_invalid_count(self):
        client = self.make_app().test_client()
        response = client.get("/task-order?next=2")

        self.assertEqual(response.status_code, 200)
        self.assertIn(b"Suggested next 2 task(s)", response.data)
        self.assertIn(b"20 points", response.data)
        self.assertIn(b"data-suggestion-id=\"SIM-003\"", response.data)
        self.assertIn(b"Use suggestion", response.data)
        self.assertEqual(client.get("/task-order?next=0").status_code, 400)
        self.assertEqual(client.get("/task-order?next=101").status_code, 400)
        self.assertEqual(client.get("/task-order?next=many").status_code, 400)

    def test_markdown_status_changes_are_observed_and_logged(self):
        client = self.make_app().test_client()
        initial = client.get("/api/items")
        movement = next(item for item in initial.json["items"] if item["id"] == "SIM-001")
        self.assertEqual(movement["status"], "in_progress")
        self.assertFalse(movement["state_has_changed"])
        self.assertIsNone(movement["state_updated_at"])
        self.assertNotIn(b"Status changed from", client.get("/history").data)

        source = self.todo / "10-sample.md"
        markdown = source.read_text(encoding="utf-8").replace(
            "- **Status:** In progress — basic slice",
            "- **Status:** Done",
        )
        source.write_text(markdown, encoding="utf-8")

        changed = client.get("/api/items")
        self.assertEqual(changed.json["summary"]["done"], 1)
        self.assertEqual(changed.json["summary"]["in_progress"], 0)
        movement = next(item for item in changed.json["items"] if item["id"] == "SIM-001")
        changed_at = movement["state_updated_at"]
        self.assertTrue(changed_at.endswith("Z"))
        self.assertTrue(movement["state_has_changed"])

        reloaded = self.make_app().test_client()
        items = reloaded.get("/api/items").json["items"]
        movement = next(item for item in items if item["id"] == "SIM-001")
        self.assertEqual(movement["status"], "done")
        self.assertEqual(movement["state_updated_at"], changed_at)
        history = reloaded.get("/history")
        self.assertEqual(history.status_code, 200)
        self.assertIn(b"Status changed from In progress to Done", history.data)
        self.assertEqual(history.data.count(b"<strong>SIM-001</strong>"), 1)

    def test_history_task_link_renders_selected_task_details(self):
        client = self.make_app().test_client()
        client.get("/api/items")
        source = self.todo / "10-sample.md"
        markdown = source.read_text(encoding="utf-8").replace(
            "- **Status:** In progress — basic slice",
            "- **Status:** Done",
        )
        source.write_text(markdown, encoding="utf-8")
        client.get("/api/items")

        history = client.get("/history")
        self.assertIn(b'href="/history?task=SIM-001#task-detail"', history.data)
        detail = client.get("/history?task=SIM-001#task-detail")
        self.assertEqual(detail.status_code, 200)
        self.assertIn(b"SIM-001 ", detail.data)
        self.assertIn(b"A server-authoritative movement loop is required.", detail.data)
        self.assertIn(b"Keep validation deterministic.", detail.data)
        self.assertIn(b"Dependent tasks", detail.data)
        self.assertIn(b"SIM-002", detail.data)
        self.assertIn(b"Network coverage.", detail.data)
        self.assertIn(b"docs/TODO/10-sample.md:3", detail.data)
        self.assertEqual(client.get("/history?task=UNKNOWN-999").status_code, 404)

    def test_history_timestamp_is_readable_in_gmt_minus_three(self):
        client = self.make_app().test_client()
        connection = sqlite3.connect(self.state_db)
        try:
            connection.execute(
                """
                INSERT INTO observed_task_state_changes
                    (task_id, old_status, new_status, description, changed_at)
                VALUES (?, ?, ?, ?, ?)
                """,
                (
                    "SIM-001",
                    "in_progress",
                    "done",
                    "Status changed from In progress to Done",
                    "2025-01-02T06:05:00Z",
                ),
            )
            connection.commit()
        finally:
            connection.close()

        response = client.get("/history")

        self.assertEqual(response.status_code, 200)
        self.assertIn(b'datetime="2025-01-02T06:05:00Z"', response.data)
        self.assertIn(b"Jan 2, 2025", response.data)
        self.assertIn("03:05 AM GMT−3".encode(), response.data)

    def test_flask_app_does_not_expose_a_status_update_endpoint(self):
        client = self.make_app().test_client()
        response = client.post(
            "/api/task-state",
            json={"task_id": "SIM-001", "status": "done"},
        )

        self.assertEqual(response.status_code, 404)
        self.assertEqual(
            next(item for item in client.get("/api/items").json["items"] if item["id"] == "SIM-001")["status"],
            "in_progress",
        )


if __name__ == "__main__":
    unittest.main()

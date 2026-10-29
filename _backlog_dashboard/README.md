# Lynx backlog dashboard

A small, local Flask app that reads the Markdown task sections under
`docs/TODO/` and shows completion/status totals, priority, source references,
acceptance criteria, and direct dependency/dependent links.
The `/dependencies` view renders the complete task graph with filters, status
colors, zoom controls, and task details. The navigation also exposes a source
TODO/FIXME/XXX triage inbox and a manager report with active, blocked, and
recommended next work.
The `/task-order` view gives a prerequisite-first route, multi-task selection,
and an editable prompt builder with acceptance criteria and relevant files. Its
next-N suggestion ranks actionable tasks by priority, in-progress continuity,
and the number of dependent tasks they unlock; unresolved external blockers are
excluded.
Prompt options control delegation/parallelism, status tracking, validation,
blocker handling, and whether to create WIP, per-task, batch, or no commits.

Run from the repository root:

```powershell
python -m pip install -r _backlog_dashboard/requirements.txt
python _backlog_dashboard/app.py
```

On Windows, install Flask once and double-click
[`run.bat`](run.bat) (or run it from a terminal) to start the dashboard.

Open <http://127.0.0.1:5000>. The app binds to loopback only and reads the
backlog on each page/API request, so Markdown updates appear after refresh.
The local server also reloads Python and template changes automatically; wait
for Flask's reload message, then refresh the browser. It intentionally leaves
the interactive Flask debugger disabled.
Task IDs used in `Depends on` and `References` are linked when a matching task
exists. Dependencies that refer to decisions or other non-task items remain
visible as text.

Completion percentage is completed tasks divided by all parsed task sections.
Statuses not recognized by the parser are counted as unspecified rather than
silently treated as incomplete. Source markers are a triage inbox, not canonical
backlog tasks. This is a read-only view of the Markdown backlog: task statuses
are changed in `docs/TODO/`, never in the Flask app.
When the dashboard next reads the backlog, it records status transitions in
`_backlog_dashboard/.data/task-state.sqlite3` and shows their UTC timestamps in
the state history. The first read establishes a baseline; changes made between
dashboard reads are detected as a transition to the latest status.
Task IDs in the history view open the parsed task details while keeping the
history list available.

Run the focused tests:

```powershell
python -m unittest discover -s _backlog_dashboard/tests -v
```

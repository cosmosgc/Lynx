# Lynx backlog dashboard

A small, local Flask app that reads the Markdown task sections under
`docs/TODO/` and shows completion/status totals, priority, source references,
acceptance criteria, and direct dependency/dependent links.
The `/dependencies` view renders the complete task graph with filters, status
colors, zoom controls, and task details.
The `/task-order` view gives a prerequisite-first route, multi-task selection,
and an editable prompt builder with acceptance criteria and relevant files.
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
Task IDs used in `Depends on` and `References` are linked when a matching task
exists. Dependencies that refer to decisions or other non-task items remain
visible as text.

Completion percentage is completed tasks divided by all parsed task sections.
Statuses not recognized by the parser are counted as unspecified rather than
silently treated as incomplete. This is a reporting view of the Markdown
backlog; it does not edit project tasks or infer status from code.

Run the focused tests:

```powershell
python -m unittest discover -s _backlog_dashboard/tests -v
```

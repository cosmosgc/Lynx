# CNT-011 — CLI expansion

- **Status:** Done
- **Completed:** 2026-11-12
- **Source task:** [CLI expansion](../50-content-editor-tooling.md#cnt-011--cli-expansion)

## Delivered

- Every dispatched command reports JSON when `--json` is supplied. Invalid
  arguments use the common `cli` failure shape and exit code 2; help and version
  requests remain plain text.
- Process exit codes distinguish success (0), invalid arguments and validation
  failures (2), compatibility failures (3), and other command failures (1).
- `lynx completions <shell> --output <path>` generates Bash, Elvish, Fish,
  PowerShell, or Zsh completions directly from the Clap command tree.
- `lynx reference --output <path>` generates Markdown help for every registered
  command and subcommand. CI regenerates it and fails when
  `docs/cli-reference.md` is stale.
- Hosted CI continues to scaffold a project and exercise its validate, build,
  run, and test commands.

## Verification

- `cargo run -p project_cli -- --json reference --output docs/cli-reference.md`
  built the CLI and generated the checked-in reference.
- The compiled CLI generated a Bash completion file and returned a JSON report.
- An invalid shell argument returned the documented `invalid_arguments` JSON
  with process exit code 2.

Project content migrations remain tracked separately in CNT-014.

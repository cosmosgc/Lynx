# ADR 0011: Internationalization and locale defaults

- **Status:** Accepted
- **Date:** 2026-10-23

## Context

The reference game targets Brazilian players, but engine code, content, and
tooling are written in English today. Adding localization after content exists
would force a rewrite of every definition, UI panel, and error path. The
project also needs a deterministic rule for which language a new project uses
and what happens when a translation is missing.

## Decision

- **Brazilian Portuguese (`pt-BR`) is the default and source language** for
  generated projects, templates, the reference game, and fallback catalogs.
- Locales use BCP 47 tags. Resolution falls back from the requested locale to
  its language, then `pt-BR`, then the key itself.
- Content definitions reference stable **message keys**; they do not embed
  player-facing text. Catalogs live in `content/localization/<locale>.ron` and
  are declared and hashed in the content manifest.
- The server sends stable codes, keys, and typed parameters. Clients render
  player-facing text. Server logs and audit records remain English.
- Accounts persist `locale` with default `'pt-BR'`; characters may override it
  for role-play and voice-over preferences.
- Formatting (dates, numbers, currency, sorting) is locale-aware on the client.
  Storage, simulation, and protocol values stay locale-neutral.
- Missing default-catalog keys are validation errors. Missing non-default keys
  are warnings during development and errors for locales declared in a release
  bundle. At runtime a missing key renders the key and logs once.
- English remains supported and is translated like any other locale.

## Consequences

- New projects start fully localized in Brazilian Portuguese, so English-first
  assumptions cannot leak into the content model.
- Translation becomes a content task with validation, hashes, and rollback,
  rather than a code change.
- Every player-facing error path must map to a catalog key; free-form English
  strings cannot be returned by the server.
- Some existing sample definitions and docs must use keys instead of literal
  names. This is a content migration, not a runtime compatibility break.
- Languages with different plural rules, scripts, or directionality require
  additional validation and font work, but the key-based model already
  accommodates them.

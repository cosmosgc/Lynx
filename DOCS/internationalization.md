# Internationalization and localization

- **Status:** Approved design; catalogs and validation land with `engine_content`.
- **Default and source language:** Brazilian Portuguese (`pt-BR`).
- **Decision record:** [`adr/0011-internationalization-and-locale-defaults.md`](adr/0011-internationalization-and-locale-defaults.md).

Lynx treats localization as part of the content pipeline, not as a client
afterthought. Every player-facing string is addressed by a stable key, and the
default project, templates, content, and runtime configuration use Brazilian
Portuguese.

## Decision summary

- **`pt-BR` is the default and source language.** New projects, the reference
  game, generated content, and fallback catalogs start in Brazilian Portuguese.
  English is an optional translation, not the source of truth.
- Locales use **BCP 47** tags (`pt-BR`, `en`, `es-419`). Tags are canonicalized
  at the boundary: lowercase language, title-case region.
- Content definitions store **message keys**, never embedded English text.
- The server sends **stable codes, keys, and typed parameters**. Clients render
  player-facing text from their locale catalog.
- The fallback chain is **requested locale → language → `pt-BR` → the key
  itself**. A missing key never panics and never blocks a frame.
- Formatting is locale-aware on the client. The server stores and compares
  locale-neutral values.
- A content bundle declares the locales it contains and hashes every catalog.

## What is localized

| Localized | Not localized |
|---|---|
| UI labels, buttons, tooltips, help | Stable IDs, component keys, protocol fields |
| Entity, item, zone, and ability names/descriptions | Log messages and metrics (English, for operators) |
| Dialogue, quest text, mail, system notices | Error codes and audit records |
| Subtitles and localized voice-over | Code identifiers, file paths, schema keys |
| Date, time, number, and currency display | Wire numbers and timestamps (UTC milliseconds) |

Gameplay never depends on the selected locale. Sorting, matchmaking, combat,
and persistence use stable IDs and numbers, so two players with different
locales see the same world.

## Locale resolution

1. Explicit account or character preference.
2. Session `locale` supplied at login (validated and canonicalized).
3. Device or platform locale, if the client opts in.
4. Project default: **`pt-BR`**.
5. Source catalog: `pt-BR`.

A request for `pt` resolves to `pt-BR`. A request for `pt-PT` falls back to
`pt-BR` until a `pt-PT` catalog exists. Unknown or malformed tags are ignored
with a diagnostic; they never fail authentication.

The effective locale is returned at login together with the fallback list so
the client can preload exactly the catalogs it needs. Locale changes apply on
the next session or through an explicit `SetLocale` request; text already on
screen is not retranslated mid-frame.

## Message keys

Keys are lowercase, dot-separated, and stable:

```text
entity.door.wooden.name
entity.door.wooden.description
ui.inventory.title
ui.inventory.capacity
dialogue.blacksmith.greeting
error.out_of_range
error.permission_denied
```

Rules:

- One key has one meaning everywhere. Never reuse a key for a different concept.
- Never build sentences by concatenating translated fragments or by appending
  `"s"` for plurals.
- Keys are content, not code. Renaming a key is a content migration.
- Namespaced keys are grouped by definition, screen, or system.
- Keys may contain typed placeholders: `{name}`, `{quantity}`, `{item}`.
- Player and character names are inserted verbatim and are never translated.

## Content catalogs

Definitions reference keys:

```ron
(
  id: "door.wooden",
  version: (major: 1, minor: 0),
  display: (
    name: "entity.door.wooden.name",
    description: "entity.door.wooden.description",
  ),
  // components, presentation, tags...
)
```

Catalogs live next to the content they describe:

```text
content/
  localization/
    pt-BR.ron
    en.ron
  entities/
  items/
  zones/
```

```ron
(
  locale: "pt-BR",
  messages: {
    "entity.door.wooden.name": "Porta de madeira",
    "entity.door.wooden.description": "Ela emperra no batente.",
    "ui.inventory.title": "Inventário",
    "ui.inventory.capacity": "Capacidade: {used} de {total}",
    "dialogue.blacksmith.greeting": "Precisa de algo forjado, {player}?",
    "error.out_of_range": "O alvo está fora de alcance.",
  },
)
```

The `pt-BR` catalog is the source. Other locales may be incomplete during
development, but a release bundle must be complete for every locale it
declares. `en` is optional and is treated like any other translation.

Pluralization uses CLDR plural categories, with `one` and `other` sufficient
for Brazilian Portuguese and `many` available for languages that need it:

```ron
"ui.inventory.items": (
  one: "{count} item",
  other: "{count} itens",
),
```

Catalog values are plain UTF-8 text. Markup is limited to a documented,
platform-neutral subset (emphasis and line breaks); clients escape everything
else. Rich text is rendered by the client, never executed.

## Server, protocol, and persistence

- Login accepts an optional `locale`; omitted means **`pt-BR`**. The response
  carries the effective locale and the fallbacks applied.
- Replication sends definition IDs, message keys, and typed parameters. It does
  not send pre-rendered localized strings for ordinary UI.
- Action failures return an `ErrorCode` and parameters. The client looks up
  `error.<code>`; the English `Display` text is for logs and diagnostics only.
- System chat notices, mail, and offline events store a message key plus JSON
  parameters. They are rendered when read, in the reader's locale.
- If the server must render text without a client (push notification, email,
  operator export), it uses the recipient's stored locale explicitly and falls
  back to `pt-BR`.
- Accounts store `locale` with default `'pt-BR'`. Characters may override it.
- Content manifests list supported locales and the SHA-256 of each catalog, so
  a client can detect a mismatched or missing translation bundle.
- Operator logs, metrics, and audit records stay in English for searchability.

## Formatting and input

- Timestamps are stored as UTC milliseconds and formatted on the client.
  Brazilian Portuguese uses `dd/MM/yyyy`, a 24-hour clock, and comma decimals.
- Currency is stored as integer minor units plus an ISO 4217 code and formatted
  by the client. The server never parses localized numbers.
- Text length limits are measured in grapheme clusters, not bytes.
- Display sorting may use a `pt-BR` collation; simulation and persistence order
  always use stable IDs and numbers, never localized names.
- Input methods, dead keys, and accented characters (`ã`, `ç`, `õ`, `é`) must
  work in every text field, including chat and character naming.

## Typography and UI

- Latin-1 coverage is mandatory. Bundle a font with full Portuguese diacritics
  and define fallbacks before adding scripts with larger glyph sets.
- No player-facing text is baked into textures. All text is rendered from
  catalogs so it can be changed without re-exporting art.
- Layouts tolerate at least 35% expansion. Buttons and labels wrap, ellipsize,
  or grow instead of clipping.
- Do not concatenate subject, verb, and object fragments. Translate complete
  messages with placeholders.
- Subtitles are always available. Voice-over is optional per locale, with
  `pt-BR` as the default recording language.

## Validation

`lynx validate` and the publishing pipeline enforce:

| Code | Meaning |
|---|---|
| `i18n.missing_key` | A referenced key is absent from a declared catalog |
| `i18n.duplicate_key` | The same key appears twice in one catalog |
| `i18n.unknown_locale` | Locale tag is malformed or not declared |
| `i18n.placeholder_mismatch` | Placeholders differ from the `pt-BR` source |
| `i18n.empty_message` | A message is empty or whitespace only |
| `i18n.invalid_markup` | Markup is outside the supported subset |
| `i18n.unused_key` | A catalog key is referenced by nothing (warning) |
| `i18n.missing_translation` | A non-default locale is incomplete (release error) |

Missing keys in the **default** catalog are errors. Missing keys in another
locale are warnings during development and errors when that locale is declared
for release. At runtime, a missing key renders the key itself and logs once; it
never panics.

## Authoring style (pt-BR)

- Address the player as **você**, with an informal but respectful tone.
- Prefer sentence case for labels and short, direct sentences for instructions.
- Avoid gendered references to the player when the character's gender is not
  known; rewrite instead of guessing.
- Keep proper nouns, brand names, and player names unchanged.
- Use Brazilian Portuguese vocabulary and spelling, not European Portuguese.
- Maintain a project glossary for recurring terms (inventory, zone, stamina,
  guild, dungeon) and keep it in the repository.
- Review translated UI in context. A string that is correct in isolation can
  still overflow a button or break a sentence.

## Workflow

1. Author source strings in `pt-BR`.
2. Run `lynx validate` to extract keys and catch missing references.
3. Translate one catalog per locale, preserving placeholders exactly.
4. Validate placeholder parity, markup, and completeness.
5. Review in-game at the smallest supported resolution and with long text.
6. Publish the bundle with locale hashes in the manifest.
7. Roll back the bundle, not individual strings, if a translation regresses.

## Testing

- Unit and integration tests use `pt-BR` as the default locale.
- A test asserts that every `ErrorCode` has an `error.<code>` catalog entry.
- Snapshot tests cover the default locale and one fallback path.
- Pseudo-locale testing (`en-XA`-style expansion) is used to catch clipping and
  concatenation before translation begins.
- Missing-key behavior is tested explicitly: render the key, log once, continue.

## Non-goals for v1

- Runtime machine translation of player chat or content.
- Right-to-left scripts (the layout model reserves room, but validation and
  font coverage come later).
- Per-zone language gating or separate language servers.
- Localized stable IDs, protocol fields, or database keys.
- Automatic translation of operator logs and audit records.

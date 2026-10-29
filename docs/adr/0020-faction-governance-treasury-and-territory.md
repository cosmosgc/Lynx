# ADR 0020: Faction governance, treasury, and territory

- **Status:** Accepted
- **Date:** 2026-10-26

## Context

The faction domain already modelled alignment, directional relations, and
per-character reputation (ADR 0009), but it had no durable membership,
leadership, treasury, or territory state. The world-simulation design requires
player guilds and NPC polities to share one rules engine, and requires map
content to stay independent from live ownership.

A naive implementation could store a treasury balance as a mutable integer, bake
ownership into zone content, or allow a faction action to partially commit. That
would make duplication, negative balances, conflicting owners, and unexplained
power values impossible to audit.

## Decision

- **One aggregate, three controllers.** `engine_core::faction::FactionWorld`
  owns faction records, membership, treasury, territory, and transfer receipts.
  `FactionController::{Player, Npc, System}` records where authority comes from;
  every controller uses the same permission and transaction rules.
- **Explicit leadership and roles.** Every faction has exactly one leader.
  Leadership moves only through `transfer_leadership`; a leader cannot leave or
  be removed without naming a successor. `GovernancePolicy` maps each role to
  permissions and per-transaction spend limits. Missing mappings grant nothing,
  and a role that may spend must declare a positive limit.
- **Append-only treasury ledger.** `Treasury` stores signed ledger entries, not a
  trusted balance. Deposits, rewards, spends, losses, and transfers are validated
  entries; the balance is their sum. Duplicate idempotency keys replay the
  original entry. A negative intermediate balance is rejected.
- **Atomic transfers.** An inter-faction transfer writes matching `transfer_out`
  and `transfer_in` entries and one receipt. The source and target treasuries are
  cloned before mutation, so a failure leaves both unchanged. A key already used
  by either ledger is a conflict rather than an accidental second movement.
- **Inspectable power.** `PowerRules` declares a rule version, base power,
  treasury weight in basis points, reserve, and cap. `PowerBreakdown` records
  every input and the result, so players and operators can explain why a faction
  has the power it does. Power never grants arbitrary combat outcomes.
- **Territory is durable ownership, not content.** A `Territory` references a
  `zone.*` definition and records the owner, contest state, claim time,
  protection expiry, cooldown expiry, and a monotonic revision. Claims, captures,
  and releases append `TerritoryChange` rows. Protection and cooldown windows
  have visible expiries; there is no permanent-immunity flag.
- **Configurable capture prerequisites.** `TerritoryRules` controls whether
  claims and captures are enabled, the protection and cooldown windows, and the
  minimum faction power required to capture. Power is evaluated by the same
  rule set exposed in the breakdown.
- **Idempotent actions.** Mutating actions take a validated action key.
  Territory changes and transfers return the original receipt on replay instead
  of applying twice. Operation keys are unique per ledger, and transfer keys are
  shared by both sides of a transfer.
- **Recovery is validating.** `FactionSnapshot` is the versioned persistence
  format. `FactionWorld::from_snapshot` rebuilds the aggregate, rejects newer
  major versions, enforces one leader per faction, rejects a member in two
  factions, rejects unknown territory owners, checks owner/history consistency,
  and requires every transfer entry to have a matching receipt.
- **Persistence and adapters.** `game_api::FactionService` exposes read-only
  faction and territory views to authenticated accounts. `FactionStore` is the
  trusted snapshot load/replace seam for the authoritative world service, not a
  client contract. The in-memory adapter stores the whole `FactionWorld`; the
  SQL migration stores the same facts in normalized tables with append-only
  ledger and history triggers.
- **Database enforcement.** `0004_factions.sql` adds the faction catalog, role
  permissions, spend limits, relations, membership, treasury ledger, transfers,
  territory ownership, and territory history. Deferred constraint triggers
  enforce one leader per faction, non-negative treasury balances, matching
  transfer entries and receipts, and territory owner/history agreement at
  transaction commit. Ledger, transfer, and history tables are append-only.

## Consequences

- Guilds and NPC polities can be compared, audited, and balanced with one set of
  rules instead of divergent code paths.
- Wealth cannot be duplicated by retries or by replaying a transfer key, and a
  balance can always be reconstructed from its ledger.
- Territory changes never rewrite map content, and ownership history survives
  content revisions.
- A malformed snapshot fails recovery instead of loading silently corrupted
  ownership or wealth.
- `FactionService` currently exposes public read-only state. Mutating faction
  commands remain authoritative simulation actions and are not part of the
  client-facing backend trait yet.
- The PostgreSQL snapshot store persists `FactionSnapshot` as part of the
  whole-world aggregate. The normalized governance and economy tables are not
  its storage representation; a future normalized adapter would need to map
  those rows to and from `FactionSnapshot` with the same validation.

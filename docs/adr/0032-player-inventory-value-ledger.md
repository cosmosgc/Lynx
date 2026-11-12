# Player inventory value ledger

- **Status:** Accepted
- **Date:** 2026-11-12

Player item movement is recorded as immutable signed quantity entries grouped
under one idempotent movement event. Each entry identifies the owner, item
definition, before/after quantity, source, actor, reason, correlation ID, and
timestamp; source and destination entries for a transfer commit with the
inventory snapshots in the same transaction. Existing inventories receive
explicit system-attributed opening-balance entries when the ledger is
introduced, so later reconciliation compares current quantities with openings
plus subsequent deltas. The current game has no currency balance, so this
ledger tracks item quantities and does not assign monetary prices.

The operator query joins character ownership at read time so ledger rows do not
duplicate account or character profile data. Existing idempotent transfer
receipts remain the request replay contract; ledger-event keys bind to those
receipts to prevent duplicate value changes. Any future grant, loss, loot, or
trade write must provide a stable key and its actor/source/reason through the
trusted inventory mutation boundary.

## Considered options

- **Reconstruct movement from current inventory snapshots:** Rejected because
  replacements lose provenance and cannot distinguish a legitimate grant from
  unexplained value creation.
- **Store only one signed delta per transfer:** Rejected because reconciliation
  needs each owner's before/after quantity and operators need to query either
  side independently.
- **Assign prices in this ledger:** Rejected because item valuation and a
  currency model do not exist yet; quantity accounting must not imply a price.

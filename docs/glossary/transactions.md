# Transactions

A **transaction** groups related data changes so they succeed together or have
no effect. If one part fails, the other parts are rolled back. This prevents
partially applied changes, such as removing an item from one inventory without
adding it to the other.

Lynx uses database transactions for operations that must keep multiple durable
records consistent, such as inventory transfers and their receipts. A
transaction is different from an atomic variable: it can protect a group of
database changes, while an atomic variable provides indivisible operations on
one small in-memory value. See [ADR 0005](../adr/0005-persistence-and-transactions.md).

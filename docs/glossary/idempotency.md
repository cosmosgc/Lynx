# Idempotency

An operation is **idempotent** when repeating the same request has the same
effect as doing it once. This matters because networks can drop a reply even
after the server completed the request. A client may retry without knowing
whether the first attempt succeeded.

For example, “transfer these items using request ID `abc`” should move them
once. If the same request arrives again, the server returns the original result
instead of moving the items a second time. A request ID alone is not enough:
the server must remember and validate the original result and who/what it
belongs to.

Lynx uses idempotency for inventory transfers, calendar commands, and other
value-changing operations. See
[PST-002](../TODO/20-persistence-backend.md#pst-002--inventory-and-transfer-ledger-persistence).

# Server authority

**Server authority** means the server is the final source of truth for game
state and decides whether requested actions are allowed. A client can ask to
move, attack, trade, or interact, but it cannot declare that the action
succeeded or choose its outcome.

For example, a client may request an attack on a target. The server checks the
actor, target, range, permissions, and rules, then updates health and tells
clients the result. This helps keep players in agreement and prevents a
modified client from simply claiming extra damage or items.

This is a core Lynx design rule. See
[security: authority invariants](../security.md#authority-invariants).

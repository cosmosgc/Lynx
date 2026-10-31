# Rate limits

A **rate limit** caps how often a client, account, or connection can perform an
operation during a period of time. It protects the server from accidental
overload and deliberate spam. A limit may be measured as a number of requests
per second, a burst allowance, or a maximum amount of work per simulation tick.

The server must enforce the limit; hiding a button in the client is not
protection because a modified client can send requests directly. Limits should
be bounded and observable, and a rejected request must not partially apply its
effects.

Lynx uses per-session and per-action budgets in its networking and simulation
paths. See [NET-002](../TODO/30-networking-security.md#net-002--rate-limiting-and-message-budgets)
and [security: abuse prevention](../security.md#abuse-prevention).

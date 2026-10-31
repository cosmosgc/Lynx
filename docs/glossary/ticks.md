# Ticks

A **tick** is one step of the server's simulation loop. During a tick the world
may process movement, actions, AI, timers, and other updates. A configured tick
interval sets how often those steps are intended to happen; for example, a
50-millisecond interval is a target of 20 ticks per second.

A tick is not necessarily the same length as a real-time frame on a player's
computer. The server owns simulation timing. If a tick takes too long, the
server needs a documented overload policy rather than silently granting extra
movement or dropping important actions.

See [SIM-018: determinism and tick budget](../TODO/10-simulation-gameplay.md#sim-018--determinism-and-tick-budget).

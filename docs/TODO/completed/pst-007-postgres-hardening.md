# PST-007 — PostgreSQL connection hardening

**Status: Done.** PostgreSQL connection setup now uses verified TLS, optional
private CA roots, bounded retries, and a bounded connection pool with a
two-second acquisition deadline. Remote plaintext is refused; loopback
plaintext requires an explicit opt-in and every resolved address must be
loopback. The world, schema inspection, and normalized stores share this
policy. Pool shutdown closes idle clients outside the pool lock.

Migration and setup scripts follow the same TLS policy, with migration
credentials separated from the runtime role. Errors avoid exposing connection
details. TLS verification is exercised with trusted, mismatched-hostname, and
untrusted-chain protocol probes; split-role setup was exercised from PowerShell
and Bash. Ambiguous queries and writes are not retried. Pool/TLS operational
monitoring remains follow-up work.

The original acceptance checklist remains in
[`20-persistence-backend.md`](../20-persistence-backend.md).

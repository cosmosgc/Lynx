# PST-006 — Durable sessions and resume windows

**Status: Done.** The backlog records this outcome: durable lifecycle and trusted revocation control delivered;. All acceptance criteria below are checked complete.

## Original task scope

- Persist session/resume metadata with hashed tokens, expiry, single-use
  rotation, and a generation/fencing value.
- Decide what may survive a restart: character attachment, last acknowledged
  tick, and interest set are candidates; open sockets obviously cannot.
- Sweep expired windows durably; revoke on logout, ban, or role change.
- Keep token material out of logs, snapshots, and debug output.

## Acceptance evidence

- [x] A reconnect after a service restart resumes within the configured window.
- [x] Replayed, expired, revoked, and already-connected tokens are rejected with
      the documented error codes.
- [x] Token rotation is atomic and survives a crash between validation and
      rotation.
- [x] Expired windows are swept without unbounded table growth.

**Implementation state:** Migrations 0014/0015 and a durable store persist
hashed token digests, verified account/character/actor metadata, expiry,
connected/revoked state, staff claim, and generation. Token consumption and
rotation are atomic; startup detaches stale connected records only under the
current zone lease. The gateway wires create/detach/rotate/revoke and restores
eligible sessions from the replayed zone, rebuilding interest and sending a
fresh snapshot. `postgres_gateway_resumes_session_after_process_restart_with_lease_fencing`
exercises this end to end against PostgreSQL, including takeover fencing,
actor restoration, token rotation, and durable principal revocation. Nakama-mode
admission routes resume credentials without trusting selection headers.
`postgres_resume_token_rotation_is_single_use_and_generation_fenced` races two
independent PostgreSQL connections and verifies exactly one atomic rotation
wins, rejects a token while its session is connected, rejects replay, and checks
the expiry sweep. `postgres_resume_rotation_survives_backend_loss_before_commit`
terminates the PostgreSQL backend after row validation and verifies rollback
leaves the prior digest claimable. Gateway tests assert `SessionExpired` for
replayed, expired, and revoked credentials and `Conflict` for an
already-connected session, and both the in-memory and PostgreSQL stores reject a
connected token without rotating or revoking it. `postgres_principal_revocation_flows_through_app_state_and_service`
drives an attached session through the trusted `AppState` event channel,
checks that the service closes its transport, and verifies PostgreSQL persisted
the revocation.
Revocation is fail-closed: the in-memory session is not removed if its durable
revoke fails, and the failure is surfaced to the service. A trusted
principal-wide revocation hook invalidates retained durable claims and local
sessions, closes active transports through the authority event
channel, and is exposed as a server-side `AppState` control method (never an
HTTP/client route). The production Nakama adapter does not currently expose a
role-change feed, so the integration receiving authoritative role updates must
invoke this method. Delivery loss after a committed rotation but before the
client receives `world.token` remains tracked separately in
[`NET-ClientReadinessReview`](../../issues/NET-ClientReadinessReview.md); PST-006
does not weaken immediate token invalidation to address it.

## Source

See [`20-persistence-backend.md`](../20-persistence-backend.md) for the full task history and implementation notes.

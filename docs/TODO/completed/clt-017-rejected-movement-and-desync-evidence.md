# CLT-017 — Rejected movement and desync evidence

**Status: Done.** All acceptance criteria below are checked complete in the source task.

## Original task scope

- Add a deterministic server/test fixture that accepts a movement request into
  the queue but rejects it during authoritative processing (for example, a
  blocked destination or an over-budget endpoint).
- Verify the local presentation returns to the authoritative position;
  server rejection must never be presented as accepted movement.
- Capture bounded desync evidence containing movement sequence, server tick,
  predicted position, and authoritative correction, with no credentials or
  private state.

## Acceptance evidence

- [x] A test drives a queued movement through server-side rejection and proves
      the authoritative position is unchanged.
- [x] A graphical or deterministic presentation test proves the correction is
      visible and the final rendered position converges to authority.
- [x] A desync diagnostic includes the input sequence and server tick (plus
      predicted/corrected positions) and is verified to redact credentials.

**Completion (2026-11-08):** The real-authority online integration test asserts
rejection, unchanged authoritative position, presentation correction/convergence,
and a bounded movement trace containing sequence, server tick, predicted and
corrected positions. The trace is credential-free and is cleared on disconnect.

## Source

See [`40-bevy-client.md`](../40-bevy-client.md) for the full task history and implementation notes.

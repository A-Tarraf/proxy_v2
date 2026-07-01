# Proxy TODO

## True I/O bandwidth (size / call-time)

Currently `Derivate` on `mpi___size___mpi_file_write_at` divides by the wall-clock
sampling window (100 ms), which underestimates bandwidth when a write call only
partially fills the window.

**Correct formula:** `Δsize / Δtime_in_write` using `mpi___time___mpi_file_write_at`
as the denominator.

**Work needed:**
- Add a server-side ratio mode to the `/trace/plot` endpoint:
  for `___size*` metrics, optionally divide by the sibling `___time*` delta instead of Δt.
- Expose the result as a selectable option in the UI (e.g. "True bandwidth" checkbox).
- Pass this corrected bandwidth series to FTIO instead of the raw size series,
  so FTIO phase detection operates on accurate throughput values.


## Event-based tracing (exact timestamps)

The proxy currently uses **sampled counters** (snapshot every 100 ms).
TMIO shows clean phase boundaries because it records individual I/O events
with precise start/end timestamps.

**Work needed:**
- New wire-protocol message: `EventRecord { fn_name, start_us, end_us, bytes }`.
- Client library emits one record per MPI file call (non-blocking).
- Proxy stores an append-only event log per job alongside the counter trace.
- New REST endpoint `/trace/events?job=X` serving the event log as JSON.
- UI renders events as exact-width rectangles on the time axis.
- Add a `--enable-event-trace` flag to the proxy to opt in (zero overhead when off).

**FTIO integration:**
- Feed event start/end times to FTIO so phase detection uses precise boundaries
  rather than the smeared sampled signal.
- FTIO already handles inter-arrival times; event records map cleanly to its input model.

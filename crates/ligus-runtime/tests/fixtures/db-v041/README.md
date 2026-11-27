# A 0.4.1 database

`ligus.db` was written by the released 0.4.1 `ligus-runtime` (tag `v0.4.1`),
with throwaway keys (device seed `[0x41; 32]`, account seed `[0x42; 32]`,
neither stored in the file). One chat in workspace
`00410000-0000-0000-0000-000000000001`:

- `q1`, answered by `r1`, streamed in three chunks that 0.4.1 then pruned
  (they are lamport witnesses only; the envelopes are in `pruned_chunks.json`);
- `q2`, answered by `r2`, whose two chunks are still stored;
- `q3`, unanswered;
- a sync cursor of 7 for `https://relay.example`.

`ids.json` names them. The migration tests in
`src/event_store/log_hygiene_tests.rs` copy it to a temp dir before opening.

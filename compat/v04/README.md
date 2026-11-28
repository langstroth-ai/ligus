# Mixed-version tests (0.4)

The released 0.4.1 build (`ligus-core` and `ligus-runtime` at the `v0.4.1`
tag) against this checkout (workspace model v3, 15 and 16.3):

- `mixed_version.rs`: rules-3 logs with `memberLeft` interleaved fold alike
  (verdicts of every pre-existing kind, the roster, devices, the rotation
  set, the chats); 0.4 reads the new kind as `Unknown`.
- `rules4.rs`:
  - L6: on rules 3, every 0.5 addition (`turnDelegated`, `memberLeft`,
    agents with `run` and `placement`, host `offers`, a completion's
    `outcome`) is inert for authz: verdicts, roster and routing match 0.4.1;
  - L7: a workspace founded on rules 4, or switched to it by an Owner, is
    read-only on 0.4.1, and nothing before the switch folds differently;
  - L8: the 0.4.1 runtime ingests and stores every 0.5 event byte for byte,
    then refuses every write on a rules-4 workspace (no message, turn claim
    or reply placeholder), so it never starts a turn or calls a model.

```sh
cargo test --manifest-path compat/v04/Cargo.toml
```

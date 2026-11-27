# Mixed-version tests (0.4)

The released 0.4.1 core (`ligus-core` at the `v0.4.1` tag) and this
checkout's core fold the same rules-3 logs, with the 0.5 kinds interleaved,
and must agree on every verdict of a pre-existing kind and on the roster
(workspace model v3, 16.3). Today that covers `memberLeft`: a 0.4 build reads
it as `Unknown`, keeps the member, and decides everything else the same.

```sh
cargo test --manifest-path compat/v04/Cargo.toml
```

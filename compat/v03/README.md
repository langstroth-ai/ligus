# Mixed-version tests (0.3)

The released 0.3.0 core (`ligus-core` at the `v0.3.0` tag) and this
checkout's core fold the same rules-2 logs, with the 0.4 kinds interleaved,
and must agree on every verdict of a pre-existing kind and on the roster
(identity model v2, test X1).

```sh
cargo test --manifest-path compat/v03/Cargo.toml
```

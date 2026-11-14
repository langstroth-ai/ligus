# Ligus 2.0 design documents

Design records for the 2.0 release (Ligus is short for *ligustica*, the
Italian honeybee; the product was formerly called Hive). They were written
in November 2026 against the `release/v2.0.0` branch. Citations are
`file:line` references into this repo (and, where prefixed `relay/`, into the
relay repo); line numbers are as of each document's date and drift as the
code moves. When a document and the code disagree, the code wins.

| Document | What it is | Status |
|---|---|---|
| [architecture-review.md](architecture-review.md) | Adversarial review of the 2.0 release candidate: architecture as built, verified root causes (RC-1 to RC-9), target architecture, invariants with required tests, sequenced refactor plan, residual risks. | Decision: **Revise**. Its gate items became the Phase 0 tracks below. |
| [phase0-specs.md](phase0-specs.md) | Implementation specs for Phase 0, one section per track: A protocol and authorization, B keys and human presence (passkeys), C host, sandbox and agents, D agent interface (ACP, Ligus MCP server), E identity service (ligus-auth). Also the merge order, migration and rollback, and the proof-test ledger. | Approved. Tracks A-D are in 2.0; Track E's client side (`ligus auth`, desktop organization sign-in) is in 2.0, with relay tenant enforcement later. Section 8 lists what is in 2.0 and what follows. |
| [identity-and-forges.md](identity-and-forges.md) | Identity without GitHub (account keys, optional linked forge identities via SSHSIG), discovery through invites and co-members, random DM ids, and a `Forge` trait for GitHub, GitLab and Forgejo/Gitea/Codeberg. | Owner decisions D1-D15 approved (section 9). In 2.0: identity decoupling, token locality, random DMs, co-member invites, the contacts client, and the forge interface with read-only forge tools for agents. Later: identity-link verification and UI, relay contact routes (relay v0.3), SourceHut and Tangled repo work (2.1-2.2), Bitbucket and Azure DevOps (on demand). |
| [directory-trust.md](directory-trust.md) | How invites and DMs avoid trusting relay-directory keys: verify against signed key publications already held, otherwise require an explicit "unverified recipient" confirmation. | Verification and confirmation are in 2.0 (`crates/hive-runtime/src/directory_trust.rs`). Signed directory records are designed and deferred to relay v0.3 / 2.x. |

The iOS-side notes for passkey presence and ligus-auth sign-in live with
the iOS app.

# Ligus 2.0 design documents

> Version note: "2.0" in this document means the Ligus 0.1.0 release (the version was reset before release).

Design records for the 2.0 release (Ligus is short for *ligustica*, the
Italian honeybee). They were written
in November 2026 against the `release/v2.0.0` branch. Citations are
`file:line` references into this repo (and, where prefixed `relay/`, into the
relay repo); line numbers are as of each document's date and drift as the
code moves. When a document and the code disagree, the code wins.

| Document | What it is | Status |
|---|---|---|
| [architecture-review.md](architecture-review.md) | Adversarial review of the 2.0 release candidate: architecture as built, verified root causes (RC-1 to RC-9), target architecture, invariants with required tests, sequenced refactor plan, residual risks. | Decision: **Revise**. Its gate items became the Phase 0 tracks below. |
| [phase0-specs.md](phase0-specs.md) | Implementation specs for Phase 0, one section per track: A protocol and authorization, B keys and human presence (passkeys), C host, sandbox and agents, D agent interface (ACP, Ligus MCP server), E identity service (ligus-auth). Also the merge order and the proof-test ledger (the owner-data migration section was deleted: 2.0 starts fresh). | Approved. Tracks A-D are in 2.0; Track E's client side (`ligus auth`, desktop organization sign-in, `crates/hive-runtime/src/ligus_auth.rs`) and `cmd/ligus-auth` in the relay repo are in 2.0, with relay tenant (`tid`) enforcement later (review P6). Section 8 lists what is in 2.0 and what follows. |
| [identity-and-forges.md](identity-and-forges.md) | Identity without GitHub (account keys, optional linked forge identities via SSHSIG), discovery through invites and co-members, random DM ids, and a `Forge` trait for GitHub, GitLab and Forgejo/Gitea/Codeberg. | Owner decisions D1-D15 approved (section 9). In 2.0: identity decoupling, token locality, random DMs, co-member invites, the contacts client, the forge interface with read-only forge tools for agents, and identity links with device-local verification against GitHub, GitLab, Forgejo and Gitea (`crates/hive-runtime/src/identity_links.rs`, Settings → Linked accounts). Later: relay contact routes (relay v0.3), SourceHut and Tangled repo work, Bitbucket and Azure DevOps (on demand). |
| [model-gateways.md](model-gateways.md) | Operator-listed LLM gateways (LiteLLM, kgateway, Envoy AI Gateway, Inference Extension pools) for serve and workers: chart allow-list and NetworkPolicy, host-offered gateway names, SA-token / API-key / mTLS auth, credential injection by the egress proxy, attribution headers. | Approved 2026-11-17; in 2.0. |
| [directory-trust.md](directory-trust.md) | How invites and DMs avoid trusting relay-directory keys: verify against signed key publications already held, otherwise require an explicit "unverified recipient" confirmation. | Verification and confirmation are in 2.0 (`crates/hive-runtime/src/directory_trust.rs`). Signed directory records are designed and deferred to relay v0.3 / 2.x. |

The iOS-side notes for passkey presence and ligus-auth sign-in live with
the iOS app.

## Owner decisions, 2026-11-16

These postdate the documents above; where a document says otherwise, these
win (and, once built, the code).

- **`ligus serve` becomes its own member** of the workspace, rather than
  an enrolled device that signs as its owner. Built in Phase B: serve and
  worker boxes are service members admitted through box admissions
  (`hive_core::admission`; operator guide, Admitting a box), and the
  enrolled-device path (`ligus serve enroll`, `LIGUS_SERVE_OWNER`) is removed.
- **Hardened phone keys.** A phone holds its own key in the Secure Enclave,
  used with Face ID; no self-enrollment; capped to a limited tier; revocable
  on its own.
- **Configurable passkey RP.** The WebAuthn relying party (today `ligus.ai`,
  hard-coded as `presence::Policy::ligus()`) becomes configurable, so a
  self-hosted deployment can use its own domain.
- **SourceHut and Tangled identity-link verifiers dropped.** Links of
  either kind fold unchanged and show as self-asserted, never verified
  (`hive_core::identity_link::DROPPED_PROVIDERS`); neither forge is offered
  for a new link.
- **Everything in 2.0.** All of the above ships in 2.0.0, not in a later
  release; security fixes found before the cut are bundled into 2.0.0 too
  ([SECURITY.md](../../../SECURITY.md)).

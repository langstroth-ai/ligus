# Review: repos-and-checkouts.md (workspace repos, per-device checkouts, coverage)

Reviewer stance: adversarial architecture review (the owner's reviewer persona).
Reviewed: `docs/design/v2/repos-and-checkouts.md` at `a1ac2dd8` (branch
`design/repos`), checked against the code in this tree (same as
`release/v2.0.0`) and against `phase0-specs.md`, `architecture-review.md`,
`identity-and-forges.md` and `directory-trust.md`. Ligus is short for
*ligustica*. Line numbers are as of this commit.

The design doc is not edited. Every finding names a required change and an
executable test.

---

## Decision

**Revise.**

The split into a shared repo identity, device-local checkouts and
host-signed coverage is the right model, and option A is the right choice
over B and C. But four parts are wrong or underspecified enough that
implementing them as written would ship a correctness or security bug:

1. **Proposal repo binding.** It defends against the wrong attack. The real
   one (an agent re-upserts its own *approved* proposal with a different
   `repo_id` and keeps its votes) is live in today's `normalize_upsert`. The
   proposed fix, digest v2, makes 2.0.0 and 2.0.x clients disagree on quorum.
2. **Ledger replay (5.6).** It treats "ordered `ProposalApplied` diffs" as the
   chat's state, but the chat's base commit isn't synced. There is no
   idempotency key, and reverts, manual commits and "applied on a different
   base" are ignored. Devices diverge silently.
3. **Canonical identity.** It is not a pure function of synced data (case
   folding depends on a device-local or Admin-hinted forge kind, IDNA is
   library-dependent, `parse_remote` doesn't validate). Aliases change
   identity but are classed as "cosmetic, no presence".
4. **Dispatch.** The "sender-side coverage gate" is advisory and can't make
   anything exactly-once. The authoritative gate is the host, and one host
   dispatch site (`crates/hive-cli/src/worker.rs`) isn't on the list. The
   proposed `stage_owner_runs_here` coverage tiebreak can't be built on an
   actor-keyed function while delegated boxes share the owner's actor
   (architecture-review 4.9).

**Phase 1 alone** (identity, repo add/remove, local mappings, clone by id,
coverage, phone section) becomes **Approve with risks** once R1, R3, R4, R5,
R6, R8 and R10 below are made. Phases 2 to 4 need the full list.

---

## Claimed invariants

- I1. Two devices that see the same remote compute the same `repo_id`
  (canonicalization is deterministic and injective enough).
- I2. A repo entry moves no bytes and grants no trust. Clones and SSRF are
  bounded by device-local forge connections.
- I3. Paths, remotes and branch names never leave a device.
- I4. Only a host can speak for its own coverage. Fake coverage can never
  make a turn run in the wrong code, because the host re-checks.
- I5. A pinned agent's turn never starts on a host without a checkout. The
  decision agrees across devices "at the sender's causal position".
- I6. An approval for a diff to repo A can never be applied to repo B.
- I7. Apply never partially applies. Any device with a checkout reaches the
  same chat state by replaying the ledger.
- I8. Two Ligus processes on one machine never share worktrees or branches.
- I9. Migration publishes nothing without a human and is idempotent.

## Hidden assumptions (not enforced by code or protocol)

- H1. The forge kind is known identically on every device. In fact it comes
  from device-local `forges.toml` or from an Admin-written hint in the event
  (2.3 step 5, 2.6).
- H2. `spec::parse_remote` output is safe to use as an identity. It does no
  host or segment validation, unlike `parse_repo_spec`
  (`crates/hive-forge/src/spec.rs:114-139`).
- H3. Remote URLs read "from the config file" and via `git config` agree.
  They don't when `include.path`/`includeIf` are present, or when a remote has
  several `url` values.
- H4. Each chat's branch on every device starts from the same base. Nothing
  synced says which base that is.
- H5. A `ProposalUpserted` for an approved proposal can't change what is
  applied without clearing votes. False for any field outside `diff_digest`
  (`crates/hive-core/src/authz/votes.rs:46-51`).
- H6. All clients in a workspace run the same digest rules. False during
  2.0.0 to 2.0.x rollout, and there is no rules-version gate in the design.
- H7. Actor id identifies a device for dispatch. False for the owner's box
  (delegated, signs as owner), already flagged as architecture-review 4.9.
- H8. `git clone` resolves hosts through Ligus's public-only resolver. It
  doesn't (identity-and-forges line 824 caveat).
- H9. `.ligus/host` is under Ligus's control. It sits in the work tree, so
  repo content or a commit can supply it.
- H10. Coverage authorized at its causal position stays valid. Revocation
  and member removal later don't retract it, because authz at causal position
  is non-retroactive by design (phase0 A.2.2 rule 13).

---

## Findings

Severity: **C** critical, **H** high, **M** medium, **L** low.

| ID | Sev | Finding | Required change | Test that would prove it |
|---|---|---|---|---|
| F1 | H | **Approved proposal can be re-pointed to another repo.** The doc's threat ("an applier picks the other repo") is the wrong one: apply reads `proposal.repo_id`. The live one: the Agent tier lets an agent re-upsert *its own* proposal (`authz/agent.rs:30-41`: incoming payload must be Open with empty approvals, which is always true of a re-upsert). `normalize_upsert` then **keeps the old approvals** because `diff_digest` is unchanged (`votes.rs:46-51`). So an agent can flip `repo_id` (or `base_commit`) on an approved proposal, and apply-on-approve (`serve/api.rs` `apply_on_this_host`) lands it in Infra. A 2.0.0 client that re-upserts (refine, title edit) drops the unknown `repo_id` and silently unbinds it, after which 8's "apply against the device's single binding" applies it anywhere. | Make `repo_id` **sticky and derived**: set from the chat's `ChatRepoSet` at the creating upsert's causal position. A later upsert that names a different `repo_id` folds as a no-op for that field, and an absent `repo_id` inherits. In `normalize_upsert`, treat a `base_commit` change as a content change (clears votes) even though the digest is unchanged. A proposal with `repo_id` is never applied via the legacy "single binding" path. | `agent_reupsert_changing_repo_id_keeps_original_repo` (property: any sequence of agent upserts never changes `repo_id` after creation); `legacy_client_upsert_without_repo_id_inherits_binding`; `base_commit_change_clears_votes`; `apply_on_approve_uses_creation_repo_only`. |
| F2 | H | **Digest v2 splits quorum across versions.** A 2.0.0 client computes v1 `diff_digest` over a proposal that a 2.0.x client votes on with a v2 digest. `authorize` denies the vote on the old client ("the vote names different content", `authz/mod.rs` vote branch), so the proposal is Approved on new clients and Open on old ones, and `ProposalApplied` is then denied on old clients (`!p.ready_to_apply()`). The projections diverge on the same event set. | Don't add `ligus-proposal-digest-v2`; F1's sticky field gives the binding without a new domain string. If the owner still wants votes to bind the repo, put it behind `WorkspaceRulesSet { version: 3 }` so old clients go read-only (`RULES_SUPPORTED`) instead of diverging, and list the string in phase0 0.1. | `mixed_version_fold_same_status`: fold the same envelopes with a v1-only and a v2 digest implementation and assert identical `ProposalStatus` and `applied` for every proposal; `rules_v3_makes_old_client_read_only`. |
| F3 | H | **Ledger replay doesn't converge.** (a) The chat base isn't synced: device A cut `hive/chat/<id>` from `main@X`, device B from `main@Y` (`chat_base` reads local `HEAD`/`hive.chatBase`), so the same ledger on different bases gives different trees. (b) There is no idempotency key: "entries its chat branch lacks" is undefined (commit subject? patch-id breaks after a 3-way merge). (c) Two devices that apply P1 concurrently produce two `ProposalApplied` events. (d) `ProposalReverted` (`events.rs:198`) isn't in the ledger. (e) "Open in editor" and "applied on a different base" commit content that isn't the approved diff, which replay can't reproduce. (f) A crash after k of N replayed commits. | Add a synced `ChatBasePinned { repo_id, commit }` (chat log, Contributor, written when the first turn cuts the branch; later changes are explicit "rebase chat" events). Every Ligus apply commit carries a `Ligus-Proposal: <uuid>` trailer, and replay skips ids already present on the branch. The ledger is `ProposalApplied` deduped by proposal id (first in canonical order) minus `ProposalReverted`. Only exactly-approved bytes enter the ledger: an editor-resolved conflict must come back as a refined proposal and be re-approved. Replay is resumable per entry. | `prop_ledger_replay_converges`: N devices, random bases pinned by event, random duplicate and reordered `ProposalApplied`/`ProposalReverted`, crash after each commit, then assert identical tree hashes; `concurrent_apply_dedupes_in_ledger`; `revert_removes_entry_on_replay`; `editor_resolution_requires_reapproval`. |
| F4 | H | **`git apply --3way` is not all-or-nothing.** On conflict it leaves conflict markers and index stages in the working tree (git documents this). Step 5 promises "nothing is applied", but step 3 runs it "onto the tip" in the chat worktree, which turns and hygiene also use. | Apply in a fresh scratch worktree at the chat tip, commit there, then `update-ref` the chat branch with compare-and-swap (expected old tip) and update the chat worktree only on success. Never run `--3way` in a shared tree. | `apply_conflict_leaves_chat_branch_and_worktree_byte_identical`; `apply_cas_fails_if_chat_tip_moved`. |
| F5 | H | **Canonical identity isn't deterministic across devices.** (a) Step 5 lowercases only for known forge kinds, but "kind" comes from per-device connections or the Admin's `forge_kind` hint, so one device lowercases `git.example.com/Org/Repo` and another doesn't, giving two ids. (b) IDNA "to ASCII" differs between UTS-46 transitional and non-transitional processing (`ß`), so library versions disagree. (c) `parse_remote` doesn't validate host or segments: spaces, `\`, `%`, `?`/`#` pass through into `RepoRef`. (d) SSH host aliases from `~/.ssh/config` (`github-work:org/repo`), very common for multi-account users, are per-device, and a workspace `host_aliases` map can't express them. (e) Scanner-reads-file and `git config` disagree on `include.path` and on multiple `url` values. (f) Nothing at fold checks that `canonical` is in normal form, so an Admin can add `github.com/ZaZulam/WeFit` with `forge_kind: ""` next to the lowercase one. | Make `canonical_repo` a pure function of (remote string, workspace-synced alias data). Case folding comes from a fixed well-known-host table (github.com, gitlab.com, codeberg.org, gitea.com) plus the repo entry's own `forge_kind`, never from `forges.toml`. Reject non-ASCII hosts (show punycode as is) instead of IDNA conversion. Run `valid_host`, `valid_segment` and `valid_repo_name` on every component. Read remotes with one function (`git config --file <gitdir>/config --no-includes --get-all`), and treat a remote with more than one `url` as ambiguous (no match). A device-local "this SSH alias means github.com" map (never synced) handles (d). At fold, `WorkspaceRepoAdded` is a no-op unless `canonical_repo(canonical) == canonical` and `id == UUIDv5(NS, canonical)`. Prefix forge canonicals with `forge:` or keep `local:` structurally distinct. | `prop_canonical_pure_and_idempotent` (no I/O, same output with any `forges.toml`); `canonical_rejects_non_ascii_backslash_space_percent`; `fold_rejects_non_normal_canonical`; `multi_url_remote_is_ambiguous`; `include_path_does_not_change_identity`; `ssh_alias_map_is_device_local`. |
| F6 | H | **Aliases change identity but are treated as cosmetic.** `WorkspaceRepoUpdated` (aliases) needs "no presence: cosmetic", and `WorkspaceHostAliasesSet` maps `ssh_host → canonical_host`. An Admin, or a compromised delegated box, maps `evil.example → github.com` or adds alias `github.com/attacker/wefit`, and any checkout of the attacker's repo now *covers* WeFit. Two repos may also claim the same alias, which makes matching ambiguous. | Alias and host-alias changes get the same floor as add: Admin plus presence under rules v2. Aliases must be globally disjoint across live repos and must not equal another repo's canonical (conflicting events fold as no-ops, first in canonical order wins). Alias-derived matches need confirmation and show "via alias" in the UI. Host aliases may map only to a host that is the canonical host of a live repo. | `alias_change_requires_presence_v2`; `alias_collision_noop_order_independent` (property over both orders); `alias_match_requires_confirmation`. |
| F7 | H | **The delegated box can author Admin repo events without a human.** The owner's `ligus serve` signs as the owner (`act_as_owner_device`; phase0 C.7). The migration and phone flows have the box author `WorkspaceRepoAdded` and `ChatRepoSet` ("the phone shows Add WeFit…", "authored by the box only after the repo exists"). Under rules v1, any paired phone bearer, or a compromised box, steers repos and chats. B.4.4's presence table and C.7's refusal list don't mention these events. | Add `WorkspaceRepoAdded/Updated/Removed` and `WorkspaceHostAliasesSet` to B.4.4 (presence under v2) and to C.7's delegated-host refusals (403 `presence_required` from phone peers until B3). Box-authored `ChatRepoSet` comes only from an explicit phone action, never from migration. | `delegated_box_cannot_add_repo_without_presence`; `phone_bearer_repo_add_refused_until_b3`; `requires_presence_exhaustive` extended to the new variants; `migration_authors_no_events`. |
| F8 | H | **Dispatch: the sender-side gate decides nothing, and a gate is missing.** For a pinned agent, nobody "dispatches": the mention syncs and the host's loop picks it up (`hive-cli/src/worker.rs:285, 318`). The sender's coverage read is a UI hint. The authoritative decision is on the host, at three sites: worker.rs, desktop `maybe_respond`, and `offer_stage_here` plus remote workflow stages. The doc lists only `send_message`/`maybe_respond`, and "agree at the sender's causal position" isn't a mechanism. Cloning→queue races `DEFERRED_CLAIM_TIMEOUT` (25 s) takeover. The `stage_owner_runs_here` coverage tiebreak is actor-keyed (`workflows.rs:562`), and actor ≠ device for delegated hosts (architecture-review 4.9); differing coverage views make two devices both claim or both defer. | State the rule: **the host's local mapping check is the only gate; coverage is advisory**. Apply it in worker.rs, `maybe_respond`, `offer_stage_here` and the remote-stage driver, before claiming. A host that is Cloning *claims* (`TurnClaimed`) immediately and then waits with a bound, so no takeover fires. Replace the tiebreak with claim-based arbitration: covering devices claim at once, others claim only after a backoff. This depends on the 4.9 device-keyed Dispatcher, so Phase 2 sequences after it. | `prop_exactly_once_dispatch` extended: N hosts, coverage flapping and stale per host, random delivery and crash points; each turn runs at most once, only on a host with a confirmed mapping, or yields one refusal; `cloning_host_claims_before_wait`; `worker_rs_refuses_without_mapping`. |
| F9 | M | **Coverage state hygiene.** (a) Authz at causal position keeps a revoked or removed host's last `Present` forever (rule 13 is non-retroactive), so dispatch UI and router keep offering it. (b) "Replaces the previous record": by what order? Reordered delivery from a relay needs a per-host sequence. (c) `CoverageState::Error(code)` as a data-carrying enum: a 2.0.x+1 variant fails deserialization of the whole event on older 2.x clients, and the host's coverage vanishes. (d) Hourly `default_head` drift puts about 8,760 events per host per year on the **config log**, which every chat event's authz frontier depends on. That adds `Hold(ConfigFrontierAhead)` churn on lagging peers. | The projection filters coverage by *current* device certification, membership and `WorkspaceHost` presence. Add `seq: u64` per host, highest wins, ties broken by event id. Make the state a string code with an `Unknown` fallback. Drop `default_head` from the log in 2.0.x (send it in the presence beat, or emit only on human-visible actions). | `coverage_from_revoked_device_dropped_after_revocation`; `coverage_highest_seq_wins_any_order`; `unknown_coverage_state_folds_as_unknown`; `coverage_event_rate_bounded`. |
| F10 | M | **"Present" proves a string, not a history.** The host re-check (5.3) compares `.git/config`'s remote string, which any directory can claim. `root_commit` is Admin-supplied and unverified, so the look-alike row in section 7 over-claims against a malicious Admin. 2.4's rule ("oldest parentless …, ties smallest sha") is date-dependent and branch-dependent and differs from `workspace_trust::first_commit` (`rev-list --max-parents=0 HEAD | last`, `workspace_trust.rs:136-148`): two fingerprint functions. | One function. Fingerprint matching is a **membership test** (`git cat-file -e <root>^{commit}` and it has no parents), not a recomputation. After a clone, verify the claimed `root_commit` and warn on mismatch. Reword the threat row so the look-alike mitigation is presence plus UI. | `fingerprint_membership_not_recompute`; `clone_verifies_claimed_root`; `one_root_commit_function` (both call sites). |
| F11 | M | **Same-machine rule as written fails three ways.** (a) `.ligus/host` lives in the work tree: a repo can ship one (naming the box's device id, which members can see) or a user can commit it, and the 0600 mode means nothing for the same OS user. (b) There is no atomic create or lock (both see "no marker" and both write), and no stale takeover (a re-enrolled device gets a new id, so the checkout is locked out forever). (c) `git clone --reference` **without `--dissociate`** makes the box clone borrow objects through alternates, so `git gc`/`prune` or deleting `~/apps/WeFit` corrupts the box's clone. | Put the marker in `$(git rev-parse --git-common-dir)/ligus-host`, created `O_EXCL` and held with `flock` while managing. Add an explicit "take over" for a marker whose device isn't live on this machine. Always use `--reference-if-able <path> --dissociate`. | `marker_in_work_tree_ignored`; `concurrent_marker_create_one_winner`; `reference_clone_survives_source_deletion`. |
| F12 | M | **Clone SSRF and confused deputy.** `git clone` resolves the host itself, so a connection that passed `check_address` at add time can be DNS-rebound to internal space before a clone-by-id (identity-and-forges line 824 admits this for pushes). Separately, "Clone there" for "a box they can reach" (5.2, Q9) means one member's request makes a box use **its owner's token** to pull a private repo, whose files then flow into turn outputs readable by every member. | Re-run `check_address` right before each clone or fetch, and pin the resolved public IP with `-c http.curloptResolve=<host>:<port>:<ip>` (git ≥ 2.37) unless the connection is `internal`. Clone by id runs only on a request from the box's owner (owner phone or owner device), or for a Worker on Admin plus presence; never from a log-carried request. | `clone_rechecks_address_and_pins_ip` (fake resolver flips to 10.0.0.1 between add and clone → refused); `clone_by_non_owner_refused`. |
| F13 | M | **Clone directory names aren't injective.** `checkout_for` joins with `__` while `_` is a legal segment character (`repos_api.rs:840-846`): `a__b/c` and `a/b__c` both give `a__b__c`, and host `h:8443` → `h-8443` collides with a host named `h-8443`. Case-insensitive APFS adds case collisions. With repo ids, two ids can map to one path. | Name new clones `<display-slug>-<first 8 hex of repo_id>`, or reject on collision, and make the mapping table enforce one repo id per path. | `checkout_dir_injective` (property over generated owner/name pairs, case-folded). |
| F14 | M | **Hidden coupling: the desktop has one global root.** `apply_chat_dir` writes a global `workspace_root` (39 `workspace_root.lock()` uses in `app/src/lib.rs`) that also drives catalog reload (repo runtimes and MCP) and the fs watcher. With per-chat repos, a turn in chat A (WeFit) while the user views chat B (Infra) can resolve B's trusted repo MCP set or watcher. | Resolve the root, catalogs and trust per turn from the chat's repo, and make the global root UI-only. Do this before Phase 2 ships. | `turn_uses_chat_repo_catalog_not_active_view` (two chats, two repos, switch view mid-turn). |
| F15 | M | **Privacy contradictions.** The 6.1 table shows "✓ via fork alice/wefit", but the coverage record carries only `via_fork: bool`; showing it would sync the fork's canonical, which I3 forbids. Repo canonicals of private repos (`github.com/acme/secret-acquisition`) are visible to every member, including Viewers. `default_head` leaks activity timing and unpushed commit ids. | Show "via a fork" only, or make the fork name opt-in per device. Document that the repo list is visible to all members. Make `default_head` off by default (F9). | `no_fork_canonical_in_synced_coverage`; `no_path_in_synced_repo_events` extended to string-pattern checks (no `/Users/`, `C:\\`, `~`). |
| F16 | M | **Migration and rollback.** "Nothing is published without a human" contradicts the box authoring `ChatRepoSet` once per chat. Making `repo-bindings.toml` and `workspace_dirs` "views" breaks a downgrade to 2.0.0 (bindings vanish). The 5.7 `root_id` change for worktrees re-keys trust, which is safe because trust is per item digest but re-prompts every user once. | Dual-write the toml and `workspace_dirs` for at least one minor release. Box chat-binding migration proposes and doesn't author. Release-note the trust re-prompt. | `downgrade_keeps_bindings`; `migration_authors_no_events` (F7). |
| F17 | L | **Fold semantics left open.** `ChatRepoSet` "hold/no-op" is ambiguous: a removed repo should deny, an unknown or ahead one should hold. Deterministic ids mean remove + re-add resurrects every chat's binding (probably desirable; say so). Concurrent `Added`/`Removed` and `Updated`/`Removed` need a stated order rule. | Specify: canonical order; remove tombstones until a later add; `ChatRepoSet` to a removed repo is denied, to an unknown one held. | `prop_repo_add_remove_update_order_independent_given_canonical_order`; `chat_repo_set_to_removed_repo_denied`. |
| F18 | L | **Phase 1's done-when can't be reached.** "A chat on the box runs in WeFit" needs a chat→checkout link, but `ChatRepoSet` is Phase 2. | Phase 1: a successful clone by id sets the box's workspace default binding when none exists (today's `RepoBindings.workspace`), and says so in the UI. | `phase1_clone_by_id_binds_workspace_default_if_empty`. |
| F19 | L | **Mis-citation and runtime scope.** `directory-trust.md` is about relay directory keys, not repo trust; the relevant controls are `workspace_trust.rs` and phase0 C.3. The 5.3 refusal must hold for every runtime that touches files (CLI agents, aider, pi, HTTP/MCP tool loops, local models' built-in file tools), so it belongs in the TurnExecutor seam, not per runtime. | Fix the citation. Put the 5.3 check in the shared turn-preparation path. | `host_refusal_applies_to_every_runtime_kind` (table over runtime kinds). |
| F20 | L | **Agent-supplied `base_commit` triggers fetches.** Step 4 fetches `base_commit` from origin. GitHub serves any commit in the fork network, so an agent-chosen sha can pull an attacker's fork objects. Low impact, but it's network I/O driven by synced, agent-authored data. | Validate 40/64 hex, fetch only on a human Apply, by sha only, through the connection's credentials and address pin. | `base_commit_fetch_only_on_human_apply`. |
| F21 | L | **`local:` identity is ambiguous.** Forks and mirrors share roots, a merged-in unrelated history adds roots, and "default branch" for a local-only repo differs per device. | `local:` repos are identified by the recorded root plus the membership test (F10). Never recompute. | `local_repo_matches_by_membership`. |
| F22 | L | **`harden_git` is narrower than the clone path assumes.** It sets hooks, fsmonitor and `protocol.ext`; the doc adds `protocol.file.allow=never` only "for clones by repo id". Fetch, pull and replay on detected checkouts should get the same set, plus `submodule.recurse=false`. | Extend `harden_git` globally, not per call site. | `harden_git_sets_file_protocol_and_submodule_overrides`. |

### Counterexamples (minimal sequences for the H findings)

**F1, re-pointed approval.** Workspace repos WeFit (W) and Infra (I).
1. Agent X files P (`repo_id = W`, diff d). 2. Two humans approve (votes
name `digest(d)`). 3. X re-upserts P with `repo_id = I`, same d, status Open,
empty approvals. That is allowed by `tier_allows` because X produced P, and
`normalize_upsert` keeps the approvals because the digest is unchanged.
4. Box with `LIGUS_SERVE_APPLY_ON_APPROVE=1` applies d into I.
*Expected:* the vote is void, or `repo_id` is immutable. *Actual (as
designed, without digest v2):* applied to I. Severity H, confidence high
(code-verified).

**F2, mixed versions.** Desktop on 2.0.x, laptop on 2.0.0. P has `repo_id`;
the desktop votes with a v2 digest. Laptop: vote denied → P Open; the
desktop's `ProposalApplied` is denied on the laptop. The two devices now
show different status forever. Severity H, confidence high.

**F3, divergent replay.** The laptop cuts the chat at `main@X`. Upstream main
advances to Y. The box cuts at `main@Y`. P1 (made on the laptop) is applied
on the laptop. The box replays P1's diff onto Y: clean, but the tree is
`Y+d1`, not `X+d1`. P2 from the box is applied on the laptop with `--3way`,
and the trees now differ in files neither proposal touched. Nothing detects
it. Severity H, confidence high.

**F8, Cloning race.** H is pinned and coverage says Cloning. H's worker sees
the mention, doesn't claim (it is "queued"), and the clone takes more than
25 s. Meanwhile the owner's desktop sees the agent as bound Elsewhere and
doesn't answer, which is fine. The router (`flow::candidates`) dropped H as
"Missing" on a peer whose fold has the older Missing record, and recruits
another agent. Two agents answer, one in the wrong code if that host's
mapping is stale. Severity H, confidence medium (depends on the router
details).

---

## Simpler alternatives

- **No new digest domain.** Derive `repo_id` from `ChatRepoSet` at proposal
  creation and keep it sticky (F1). One fold rule, no mixed-version split,
  and phase0 0.1 stays closed.
- **Coverage off the config log.** Coverage is advisory once the host check
  is authoritative (F8). It could ride the relay presence beat (ephemeral,
  like `host_online`), with a signed log record only for state changes. That
  removes config-log growth and authz frontier churn (F9).
- **Phase 1 without scanning outside `repos_dir`.** The box always clones
  its own copy (Q3/Q5 answers below), which removes the same-machine marker
  from Phase 1 entirely. The only path into a foreign checkout is the
  existing `ligus bind-repo /abs/path`, which should then refuse a path whose
  git-common-dir has another host's marker.
- **Ledger replay deferral.** Until ChatBasePinned and trailers exist,
  "Apply on…" (5.5 step 1, routing apply to the producing host) gives
  cross-device apply without replay. Ship that first, and replay in 2.2.

---

## Required changes before approval

Phase 1 (2.0.x) blockers:
- R1. `canonical_repo` as a pure function, with fold-time normal-form check,
  strict charsets, no IDNA, well-known-host case table, single remote reader
  (F5).
- R2. Aliases and host aliases: presence, disjointness, confirmation (F6).
  Or drop aliases from Phase 1 (they're Phase 4 anyway); then the
  `WorkspaceRepoUpdated` payload in Phase 1 must not carry them.
- R3. Repo events in B.4.4 and the C.7 delegated refusals; migration authors
  nothing (F7, F16).
- R4. Coverage: current-state revocation filter, per-host `seq`,
  string-coded state, no `default_head` on the log (F9).
- R5. Clone by id: owner-only requester, address re-check and IP pin,
  injective directory names, `--dissociate` (F12, F13, F11c).
- R6. Phase 1 binds the clone as the box's workspace default (F18).
- R7. Fix the directory-trust citation and the "via fork" privacy
  contradiction (F15, F19).

Phase 2 blockers:
- R8. The host-local check is the sole gate, in worker.rs, `maybe_respond`,
  `offer_stage_here` and remote stages; claim-before-wait; sequence after the
  4.9 device-keyed Dispatcher; drop the `stage_owner_runs_here` tiebreak (F8).
- R9. Per-turn root, catalog and trust resolution on the desktop (F14).
- R10. Repo fold order rules and `ChatRepoSet` deny/hold split (F17).

Phase 3 blockers:
- R11. Sticky derived `repo_id`; no digest v2 (or rules v3 gating) (F1, F2).
- R12. Scratch-worktree apply with CAS ref update (F4).
- R13. ChatBasePinned, apply trailers, deduped ledger with reverts,
  approved-bytes-only, resumable replay (F3).
- R14. One fingerprint function with a membership test (F10, F21).

## Residual risks (after the revisions)

- Coverage remains self-reported. A host can claim Present and then refuse
  turns, which is a liveness nuisance, not a safety issue.
- SSH-alias users need a device-local alias step before their checkouts
  match. Some will see "Missing" and clone a second copy.
- Case-preserving unknown forges can split one repo into two entries until
  an Admin cleans it up (the Q10 trade).
- Ledger replay still can't reproduce commits made outside Ligus in the
  chat worktree. Those stay device-local and must be flagged, not synced.
- The repo list is workspace-visible metadata. E2EE hides it from the relay,
  not from members.
- DNS pinning narrows but doesn't close rebinding when a proxy resolves
  (`HTTP(S)_PROXY`), as identity-and-forges already notes.

---

## Answers to the open questions

1. **Q1, MVP scope.** Ship Phase 1 alone in a 2.0.x, trimmed per R1–R7. It
   changes no dispatch path, so a pinned agent running repo-less on a box is
   today's behaviour, not a regression. Dispatch refusal must not land
   piecemeal: Phase 2 goes in 2.1 after the 4.9 Dispatcher, because a
   half-gated dispatch (sender hint without host refusal) is worse than
   none.
2. **Q2, presence.** Admin plus presence under rules v2 for add, remove,
   **and alias changes**. Repos are added rarely, so one tap is cheap, and
   this is the only control against a compromised Admin device or a
   delegated box steering clones (F6, F7). Display name and default branch
   edits can stay presence-free.
3. **Q3, box scan roots.** Only `repos_dir` plus roots added on the box's
   CLI. No phone-picked roots in 2.0.x: even "names only" teaches the phone
   the box's directory layout, and it's an un-needed write path through a
   bearer. On the owner's Mac, the box should keep its own clones and not see
   `~/apps`.
4. **Q4, multi-repo chats.** One repo per chat for all of 2.x. Multi-repo
   turns need per-repo proposals, grouped approval and atomic multi-repo
   apply, which is a different design. Use one chat per repo plus hand-off
   for now.
5. **Q5, same-machine sharing.** Always a separate clone
   (`--reference-if-able … --dissociate`). Read-only use still needs the
   marker, detached worktrees inside someone else's `.ligus/worktrees` and a
   refusal path, which is a lot of mechanism for disk-space savings.
6. **Q6, coverage default.** On for Worker hosts and for the account owner's
   own devices in workspaces they own; off (shown as "unknown") for other
   members' personal devices until they opt in. `default_head` off
   everywhere by default.
7. **Q7, digest v2.** No. Bind through a sticky `repo_id` derived from
   `ChatRepoSet` at creation (F1). This closes the actual attack and avoids
   the mixed-version quorum split (F2). Revisit only together with a rules v3
   bump.
8. **Q8, chat branches.** Not ledger replay as designed. First add a synced
   chat base, apply trailers and a deduped ledger (F3). Until then, "Apply
   on…" the producing host. Forge push of `hive/chat/<id>` stays an optional
   accelerator. Rename `hive/` to `ligus/` in the same release as ChatBasePinned,
   reading both prefixes for one release so hygiene doesn't orphan old trees.
9. **Q9, driving another host.** Phone-only (owner's paired phone) in 2.0.x.
   Desktop to box later, only over the owner's own tunnel and only by the
   box owner. Never as a request carried on the log: that turns the config
   log into an RPC bus that lets any member spend the box owner's token
   (F12).
10. **Q10, unknown forges.** Keep case-sensitive. A split is a visible UX
    problem fixable with an alias. A merge silently treats two different
    repos as one, which breaks I1 and I6. Case folding must be decided by a
    fixed host table or the repo entry's `forge_kind`, never by a device's
    connections (F5).

## Scope realism

The doc is four phases of real work. Phase 1 is M to L (new event family,
authz, bounds, a shared `hive-runtime` table, clone by id, a phone section).
Phase 2 is L and blocked on the 4.9 Dispatcher plus the desktop global-root
refactor (F14). Phase 3 is L to XL: ledger replay is the deferred 2b and is
the riskiest piece. Phase 4 is M. A realistic plan: Phase 1 in 2.0.x,
Phase 2 in 2.1, Phase 3 in 2.2 at the earliest, Phase 4 after. Promising
cross-device apply in 2.0 or 2.1 would not be realistic.

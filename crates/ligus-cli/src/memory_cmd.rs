//! `ligus memory …` and `ligus task-state <chat>`: workspace memory and
//! live task state from the command line (`ligus_core::memory`; design
//! `docs/design/v2/workspace-memory.md`). Writes are this CLI identity's,
//! judged by the fold like any device's: a person's identity writes and
//! decides; a box (a service member) can only read and propose.

use ligus_runtime::display;
use std::io::Read;

use anyhow::{anyhow, bail, Result};
use ligus_core::memory::{self, MemoryEntry, MemoryOp, MemorySource, MemoryState, ProposalState, TaskSlot};
use ligus_runtime::chat_service::MemoryDraft;
use uuid::Uuid;

use crate::{open_service, uuid_of_room, Config};

/// The flags `ligus memory` reads.
#[derive(Debug, Default, Clone)]
pub struct Flags {
    pub tags: Vec<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub note: Option<String>,
    pub base: Option<String>,
    pub json: bool,
}

pub const USAGE: &str = "usage: ligus memory list | search <words…> | show <id> | add <title> [--body TEXT | stdin] [--tag a,b] | \
edit <id> [--title T] [--body TEXT | stdin] [--tag a,b] [--base REV] | rm <id> [--base REV] | proposals | accept <proposal-id> | reject <proposal-id> [--note TEXT]   (--json for machine output)";

fn id(s: &str) -> Result<Uuid> {
    Uuid::parse_str(s.trim()).map_err(|_| anyhow!("not an id: {s}"))
}

fn stdin_body() -> Result<String> {
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!("type the body, then Ctrl-D on a new line (or pass --body TEXT, or pipe it)");
    }
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

/// One entry as a list line.
pub fn entry_line(e: &MemoryEntry) -> String {
    let e = &display::memory_entry(e);
    let tags = if e.current.tags.is_empty() { String::new() } else { format!(" [{}]", e.current.tags.join(", ")) };
    let conflicts = if e.conflicts.is_empty() { String::new() } else { format!(" ({} conflict{})", e.conflicts.len(), if e.conflicts.len() == 1 { "" } else { "s" }) };
    format!("{}  {}{tags}{conflicts}  by {}", e.id, e.current.title, e.current.author.name)
}

/// One entry in full.
pub fn entry_text(e: &MemoryEntry) -> String {
    let e = &display::memory_entry(e);
    let c = &e.current;
    let mut out = format!("{}\nid {}  rev {}  versions {}\nby {} ({:?})", c.title, e.id, c.rev, e.revision_count, c.author.name, c.author.kind);
    if !c.proposer.is_empty() {
        out.push_str(&format!(", proposed by agent {}", c.proposer));
    }
    if let Some(chat) = c.source.chat_id {
        out.push_str(&format!(", from chat {chat}"));
    }
    if !c.tags.is_empty() {
        out.push_str(&format!("\ntags {}", c.tags.join(", ")));
    }
    out.push_str(&format!("\n\n{}\n", c.body.trim_end()));
    for k in &e.conflicts {
        let what = if k.removal { "removed it" } else { "edited an earlier version" };
        out.push_str(&format!("\nconflict rev {}: {} {what}\n", k.rev, k.author.name));
        if !k.removal {
            out.push_str(&format!("{}\n", k.body.trim_end()));
        }
    }
    out
}

/// One proposal as a list line.
pub fn proposal_line(p: &ProposalState) -> String {
    let p = &display::memory_proposal(p);
    let state = match &p.decision {
        None => "open".to_string(),
        Some(d) if d.accepted => format!("accepted by {}", d.by.name),
        Some(d) => format!("rejected by {}", d.by.name),
    };
    let what = match &p.proposal.op {
        MemoryOp::Create { title, .. } => format!("create \u{201c}{title}\u{201d}"),
        MemoryOp::Edit { entry_id, title, .. } => format!("edit {entry_id} \u{201c}{title}\u{201d}"),
        MemoryOp::Remove { entry_id, .. } => format!("remove {entry_id}"),
    };
    let by = if p.proposal.proposer.is_empty() { p.by.name.clone() } else { format!("agent {}", p.proposal.proposer) };
    format!("{}  {what}  by {by}  [{state}]", p.proposal.proposal_id)
}

/// One task-state slot as text.
pub fn slot_text(s: &TaskSlot, now_ms: i64) -> String {
    let u = &s.update;
    let state = if s.active(now_ms) { "" } else if s.stale(now_ms) { " (stale)" } else { " (finished)" };
    let mut out = format!(
        "{}  {}{}{state}  {}",
        u.agent_key,
        u.status.as_str(),
        u.progress.map(|p| format!(" {p}%")).unwrap_or_default(),
        u.summary
    );
    for q in &u.open_questions {
        out.push_str(&format!("\n    ? {q}"));
    }
    for r in &u.partial_results {
        out.push_str(&format!("\n    = {r}"));
    }
    out
}

fn find<'a>(m: &'a MemoryState, raw: &str) -> Result<&'a MemoryEntry> {
    let id = id(raw)?;
    m.entries.get(&id).filter(|e| e.live()).ok_or_else(|| anyhow!("no live memory entry {id}"))
}

pub fn run(cfg: &Config, positional: &[&String], f: Flags) -> Result<()> {
    let mut svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let arg = |i: usize| positional.get(i).map(|s| s.as_str());
    let print_json = |v: serde_json::Value| println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    match arg(0) {
        Some("list") | Some("search") | None => {
            let words: Vec<&str> = if arg(0) == Some("search") { positional[1..].iter().map(|s| s.as_str()).collect() } else { vec![] };
            let m = svc.memory(ws)?;
            let hits = memory::search(&m, &words.join(" "), &f.tags, memory::MAX_LIVE_ENTRIES);
            if f.json {
                print_json(serde_json::json!({ "total": m.live_count(), "entries": hits.iter().map(|h| display::memory_entry(h.entry)).collect::<Vec<_>>() }));
            } else if hits.is_empty() {
                println!("(no notes{})", if words.is_empty() { "" } else { " match every word" });
            } else {
                for h in hits {
                    println!("{}", entry_line(h.entry));
                }
            }
        }
        Some("show") => {
            let m = svc.memory(ws)?;
            let e = find(&m, arg(1).ok_or_else(|| anyhow!("usage: ligus memory show <id>"))?)?;
            if f.json {
                print_json(serde_json::to_value(display::memory_entry(e))?);
            } else {
                print!("{}", entry_text(e));
            }
        }
        Some("add") => {
            let title = positional[1..].iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
            if title.trim().is_empty() {
                bail!("usage: ligus memory add <title> [--body TEXT | stdin] [--tag a,b]");
            }
            let body = match f.body { Some(b) => b, None => stdin_body()? };
            let (entry, rev) = svc.write_memory(ws, MemoryDraft { title, body, tags: f.tags, ..Default::default() })?;
            println!("{entry}  rev {rev}");
        }
        Some("edit") => {
            let m = svc.memory(ws)?;
            let e = find(&m, arg(1).ok_or_else(|| anyhow!("usage: ligus memory edit <id> …"))?)?.clone();
            let base = match f.base { Some(b) => id(&b)?, None => e.head() };
            let body = match f.body { Some(b) => b, None => stdin_body()? };
            let tags = if f.tags.is_empty() { e.current.tags.clone() } else { f.tags };
            let draft = MemoryDraft {
                entry_id: Some(e.id),
                base: Some(base),
                title: f.title.unwrap_or_else(|| e.current.title.clone()),
                body,
                tags,
                source: MemorySource::default(),
                resolves: if base == e.head() { e.conflicts.iter().map(|c| c.rev).collect() } else { vec![] },
            };
            let (_, rev) = svc.write_memory(ws, draft)?;
            println!("{}  rev {rev}", e.id);
        }
        Some("rm") => {
            let m = svc.memory(ws)?;
            let e = find(&m, arg(1).ok_or_else(|| anyhow!("usage: ligus memory rm <id>"))?)?.clone();
            let base = match f.base { Some(b) => id(&b)?, None => e.head() };
            svc.remove_memory(ws, e.id, base, vec![])?;
            println!("removed {} (its history stays in the workspace log)", e.id);
        }
        Some("proposals") => {
            let m = svc.memory(ws)?;
            let mut ps: Vec<&ProposalState> = m.proposals.values().collect();
            ps.sort_by(|a, b| a.decision.is_some().cmp(&b.decision.is_some()).then(b.at_ms.cmp(&a.at_ms)));
            if f.json {
                print_json(serde_json::to_value(ps.iter().map(|p| display::memory_proposal(p)).collect::<Vec<_>>())?);
            } else if ps.is_empty() {
                println!("(no memory proposals)");
            } else {
                for p in ps {
                    println!("{}", proposal_line(p));
                }
            }
        }
        Some(verb @ ("accept" | "reject")) => {
            let pid = id(arg(1).ok_or_else(|| anyhow!("usage: ligus memory {verb} <proposal-id>"))?)?;
            svc.decide_memory_proposal(ws, pid, verb == "accept", f.note.as_deref().unwrap_or(""), vec![])?;
            println!("{verb}ed {pid}");
        }
        Some(_) => bail!("{USAGE}"),
    }
    Ok(())
}

/// `ligus task-state <chat-id> [--json]`.
pub fn task_state(cfg: &Config, chat: Option<&str>, json: bool) -> Result<()> {
    let chat = id(chat.ok_or_else(|| anyhow!("usage: ligus task-state <chat-id> [--json]"))?)?;
    let svc = open_service(cfg)?;
    let slots = svc.task_state(chat)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&slots.iter().map(display::task_slot).collect::<Vec<_>>())?);
        return Ok(());
    }
    if slots.is_empty() {
        println!("(no task state on this chat)");
    }
    let now = ligus_core::flow::now_ms();
    for s in &slots {
        println!("{}", slot_text(s, now));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ligus_core::memory::{MemoryAuthor, MemoryRevision, TaskStateUpdate, TaskStatus};

    #[test]
    fn lines_show_provenance_conflicts_and_state() {
        let ana = MemoryAuthor { id: "ana".into(), name: "Ana".into(), kind: ligus_core::ActorKind::Human };
        let rev = MemoryRevision { rev: Uuid::from_u128(2), title: "Relay ports".into(), body: "8443".into(), tags: vec!["infra".into()], author: ana.clone(), at_ms: 1, proposer: "coder".into(), ..Default::default() };
        let e = MemoryEntry { id: Uuid::from_u128(1), current: rev.clone(), created_by: ana.clone(), revision_count: 2, conflicts: vec![MemoryRevision { removal: true, ..rev }], ..Default::default() };
        assert!(entry_line(&e).ends_with("Relay ports [infra] (1 conflict)  by Ana"));
        let t = entry_text(&e);
        assert!(t.contains("proposed by agent coder") && t.contains("Ana removed it") && t.contains("\n\n8443\n"), "{t}");
        let s = TaskSlot {
            update: TaskStateUpdate { agent_key: "coder".into(), status: TaskStatus::Blocked, summary: "need a call".into(), progress: Some(30), open_questions: vec!["v1?".into()], ..Default::default() },
            at_ms: 0,
            ..Default::default()
        };
        assert_eq!(slot_text(&s, 1), "coder  blocked 30%  need a call\n    ? v1?");
        assert!(slot_text(&s, i64::MAX).contains("(stale)"));
    }

    /// Apps residuals: a peer's memory title or name reached the terminal
    /// raw, so an RLO reordered the line.
    #[test]
    fn titles_and_names_print_cleaned() {
        let evil = "Ana\u{202E}gnp.exe\u{2028}x\u{200B}";
        let who = MemoryAuthor { id: "ana".into(), name: evil.into(), kind: ligus_core::ActorKind::Human };
        let rev = MemoryRevision { title: evil.into(), body: "b".into(), author: who.clone(), ..Default::default() };
        let e = MemoryEntry { current: rev.clone(), created_by: who.clone(), conflicts: vec![rev], ..Default::default() };
        let p = ProposalState {
            proposal: ligus_core::memory::MemoryProposal {
                proposal_id: Uuid::nil(),
                op: MemoryOp::Create { entry_id: Uuid::nil(), title: evil.into(), body: String::new(), tags: vec![] },
                proposer: String::new(),
                source: MemorySource::default(),
                rationale: String::new(),
            },
            by: who,
            signer: None,
            at_ms: 0,
            decision: None,
        };
        for out in [entry_line(&e), entry_text(&e), proposal_line(&p)] {
            assert!(!out.contains('\u{202E}') && !out.contains('\u{200B}') && !out.contains('\u{2028}'), "{out:?}");
        }
        assert!(entry_line(&e).contains("Anagnp.exe x"));
    }
}

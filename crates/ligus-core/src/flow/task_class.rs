//! Task classes: a short label grouping work for routing scores and reports
//! (`refactor`, `docs`, `review`, …). A class is a grouping, not a quality
//! claim, so a cheap deterministic keyword match is enough — the same text
//! gives the same class on every device.

/// Every class this build assigns, most specific first.
pub const CLASSES: &[&str] = &[
    TRIAGE, REVIEW, TEST, DOCS, REFACTOR, BUGFIX, FEATURE, SUMMARIZE, OPS, QUESTION, GENERAL,
];

pub const TRIAGE: &str = "triage";
pub const REVIEW: &str = "review";
pub const TEST: &str = "test";
pub const DOCS: &str = "docs";
pub const REFACTOR: &str = "refactor";
pub const BUGFIX: &str = "bugfix";
pub const FEATURE: &str = "feature";
pub const SUMMARIZE: &str = "summarize";
pub const OPS: &str = "ops";
pub const QUESTION: &str = "question";
pub const GENERAL: &str = "general";

/// Classes that carry little risk: a new agent may take them before it has a
/// track record (see [`super::career`]).
pub const LOW_RISK: &[&str] = &[TRIAGE, REVIEW, TEST, DOCS, SUMMARIZE, QUESTION, GENERAL];

pub fn is_low_risk(class: &str) -> bool {
    LOW_RISK.contains(&class)
}

/// Keyword table: a class wins when any of its words appears as a word (or
/// word prefix) in the text. Checked in order, so `review the refactor` is a
/// review.
const RULES: &[(&str, &[&str])] = &[
    (
        TRIAGE,
        &[
            "triage",
            "flaky",
            "investigate",
            "repro",
            "reproduce",
            "root cause",
            "why is",
            "why does",
        ],
    ),
    (
        REVIEW,
        &[
            "review",
            "critique",
            "feedback on",
            "look over",
            "sanity check",
            "audit",
        ],
    ),
    (
        TEST,
        &[
            "test",
            "tests",
            "unit test",
            "coverage",
            "spec for",
            "e2e",
            "assert",
        ],
    ),
    (
        DOCS,
        &[
            "doc",
            "docs",
            "readme",
            "documentation",
            "changelog",
            "comment",
            "docstring",
            "tutorial",
            "guide",
        ],
    ),
    (
        REFACTOR,
        &[
            "refactor",
            "rename",
            "clean up",
            "cleanup",
            "simplify",
            "extract",
            "restructure",
            "deduplicate",
            "move ",
        ],
    ),
    (
        BUGFIX,
        &[
            "fix",
            "bug",
            "broken",
            "crash",
            "error",
            "regression",
            "fails",
            "failing",
            "panic",
        ],
    ),
    (
        FEATURE,
        &[
            "add",
            "implement",
            "build",
            "create",
            "support",
            "feature",
            "new endpoint",
            "introduce",
        ],
    ),
    (
        SUMMARIZE,
        &[
            "summarize",
            "summarise",
            "summary",
            "tl;dr",
            "tldr",
            "recap",
            "explain this pr",
            "digest",
        ],
    ),
    (
        OPS,
        &[
            "deploy",
            "release",
            "ci",
            "pipeline",
            "docker",
            "kubernetes",
            "k8s",
            "infra",
            "migrate",
            "upgrade",
        ],
    ),
    (
        QUESTION,
        &[
            "?",
            "how do",
            "what is",
            "where is",
            "can you explain",
            "explain",
        ],
    ),
];

fn has_word(text: &str, word: &str) -> bool {
    if word == "?" {
        return text.trim_end().ends_with('?');
    }
    if word.contains(' ') || word.ends_with(' ') {
        return text.contains(word);
    }
    // Word-start match: "doc" matches "docs"/"documented" but not "adoc".
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(pos) = text[from..].find(word) {
        let at = from + pos;
        let start_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        if start_ok {
            return true;
        }
        from = at + word.len();
    }
    false
}

/// Strip `@mentions`, code fences and inline code so a pasted stack trace
/// ("error", "panic") doesn't decide the class of a docs request.
fn prose(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut in_code = false;
        for word in line.split_inclusive(char::is_whitespace) {
            let w = word.trim();
            if w.starts_with('@') {
                continue;
            }
            for c in word.chars() {
                if c == '`' {
                    in_code = !in_code;
                } else if !in_code {
                    out.push(c);
                }
            }
        }
        out.push('\n');
    }
    out.to_lowercase()
}

/// Classify a task from its text.
pub fn classify(text: &str) -> &'static str {
    let p = prose(text);
    if p.trim().is_empty() {
        return GENERAL;
    }
    for (class, words) in RULES {
        if words.iter().any(|w| has_word(&p, w)) {
            return class;
        }
    }
    GENERAL
}

/// Classify a proposal: by the files it touches when they settle it (only
/// docs → `docs`, only tests → `test`), else by its title and body.
pub fn classify_proposal(title: &str, body: &str, changed_files: &[String]) -> &'static str {
    if !changed_files.is_empty() {
        if changed_files.iter().all(|f| is_docs_path(f)) {
            return DOCS;
        }
        if changed_files.iter().all(|f| is_test_path(f)) {
            return TEST;
        }
    }
    match classify(&format!("{title}\n{body}")) {
        QUESTION | GENERAL if !changed_files.is_empty() => FEATURE,
        c => c,
    }
}

pub fn is_docs_path(path: &str) -> bool {
    let p = path.to_lowercase();
    p.ends_with(".md")
        || p.ends_with(".mdx")
        || p.ends_with(".rst")
        || p.ends_with(".txt")
        || p.starts_with("docs/")
        || p.contains("/docs/")
        || p.starts_with("docs-site/")
}

pub fn is_test_path(path: &str) -> bool {
    let p = path.to_lowercase();
    p.starts_with("tests/")
        || p.contains("/tests/")
        || p.contains("/test/")
        || p.contains("__tests__")
        || p.ends_with("_test.go")
        || p.ends_with("_test.rs")
        || p.ends_with(".test.ts")
        || p.ends_with(".test.tsx")
        || p.ends_with(".spec.ts")
        || p.ends_with("_test.py")
        || p.rsplit('/').next().is_some_and(|f| f.starts_with("test_"))
}

/// Whether a shell command line runs a test suite (`cargo test`, `npm test`,
/// `pytest`, …). Used to read a turn's measured test result from the exit
/// status of the command the harness ran.
pub fn is_test_command(command: &str) -> bool {
    let c = command.to_lowercase();
    [
        "cargo test",
        "cargo nextest",
        "npm test",
        "npm run test",
        "pnpm test",
        "yarn test",
        "bun test",
        "bun run test",
        "vitest",
        "jest",
        "pytest",
        "python -m pytest",
        "go test",
        "mvn test",
        "gradle test",
        "./gradlew test",
        "make test",
        "ctest",
        "dotnet test",
        "mix test",
        "rspec",
    ]
    .iter()
    .any(|t| c.contains(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_requests() {
        assert_eq!(
            classify("Please refactor the parser into modules"),
            REFACTOR
        );
        assert_eq!(classify("fix the crash when the relay is down"), BUGFIX);
        assert_eq!(classify("write unit tests for the scheduler"), TEST);
        assert_eq!(classify("update the README with install steps"), DOCS);
        assert_eq!(classify("review PR 42"), REVIEW);
        assert_eq!(classify("summarize this thread"), SUMMARIZE);
        assert_eq!(classify("how do workers pick up stages?"), QUESTION);
        assert_eq!(classify("hello"), GENERAL);
        assert_eq!(classify(""), GENERAL);
    }

    #[test]
    fn ignores_mentions_and_code() {
        // A pasted panic in a code block doesn't make a docs request a bugfix.
        let text = "@writer document this API\n```\nthread 'main' panicked: error\n```";
        assert_eq!(classify(text), DOCS);
        assert_eq!(classify("@fixer `fix()` explain what it does"), QUESTION);
    }

    #[test]
    fn word_start_matching() {
        assert!(has_word("see the docs", "doc"));
        assert!(!has_word("asciidoc", "doc"));
    }

    #[test]
    fn proposal_classes_follow_the_files() {
        let docs = vec!["README.md".to_string(), "docs/a.md".to_string()];
        assert_eq!(classify_proposal("Tidy", "", &docs), DOCS);
        let tests = vec!["crates/x/tests/a.rs".to_string()];
        assert_eq!(classify_proposal("Tidy", "", &tests), TEST);
        let code = vec!["src/lib.rs".to_string()];
        assert_eq!(classify_proposal("Rename the parser", "", &code), REFACTOR);
        assert_eq!(classify_proposal("Changes", "", &code), FEATURE);
        assert_eq!(classify_proposal("Pick a name?", "", &[]), QUESTION);
    }

    #[test]
    fn recognises_test_commands() {
        assert!(is_test_command("cd app && cargo test -p ligus-core"));
        assert!(is_test_command("bun run test"));
        assert!(is_test_command("python -m pytest -q"));
        assert!(!is_test_command("cargo build"));
    }

    #[test]
    fn every_rule_class_is_listed() {
        for (c, _) in RULES {
            assert!(CLASSES.contains(c));
        }
        for c in LOW_RISK {
            assert!(CLASSES.contains(c));
        }
    }
}

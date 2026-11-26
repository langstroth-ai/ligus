//! Signed JSON must serialize deterministically: no `HashMap`/`HashSet` in a
//! type that derives `Serialize` (their iteration order is random per process,
//! so a re-serialization would not match the signed bytes). Scans
//! `src/**/*.rs` for such fields inside `#[derive(...Serialize...)]` items.
//!
//! `device_link::DeviceLinks` holds a `HashMap` but is not serializable (it is
//! derived from the config projection on each device), so it is out of scope.

use std::path::Path;

const ALLOWED: &[&str] = &[];

fn rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// (item name, item text) for every struct/enum whose derive list has Serialize.
fn serialized_items(src: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].trim();
        if l.starts_with("#[derive(") && l.contains("Serialize") {
            // Find the item line, then its body up to the matching close.
            let mut j = i + 1;
            while j < lines.len() && !(lines[j].contains("struct ") || lines[j].contains("enum ")) {
                j += 1;
            }
            if j >= lines.len() {
                break;
            }
            let head = lines[j];
            let name = head
                .split_whitespace()
                .skip_while(|w| *w != "struct" && *w != "enum")
                .nth(1)
                .unwrap_or("")
                .trim_end_matches(['{', '(', ';', '<'])
                .split('<')
                .next()
                .unwrap()
                .to_string();
            let mut depth = 0i32;
            let mut body = String::new();
            let mut k = j;
            loop {
                let line = lines[k];
                body.push_str(line);
                body.push('\n');
                depth += line.matches(['{', '(']).count() as i32;
                depth -= line.matches(['}', ')']).count() as i32;
                if depth <= 0 && (line.contains('}') || line.trim_end().ends_with(';')) {
                    break;
                }
                k += 1;
                if k >= lines.len() {
                    break;
                }
            }
            out.push((name, body));
            i = k + 1;
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn no_hashmap_in_signed_types() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    let mut offenders = Vec::new();
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap();
        let file = f.file_name().unwrap().to_string_lossy().to_string();
        for (name, body) in serialized_items(&src) {
            if body.contains("HashMap<") || body.contains("HashSet<") {
                let id = format!("{file}:{name}");
                if !ALLOWED.contains(&id.as_str()) {
                    offenders.push(id);
                }
            }
        }
    }
    assert!(offenders.is_empty(), "HashMap/HashSet in serialized types: {offenders:?}");
}

#[test]
fn the_scanner_finds_a_planted_offender() {
    let src = "#[derive(Debug, Serialize)]\npub struct Bad {\n    m: HashMap<String, u8>,\n}\n";
    let items = serialized_items(src);
    assert_eq!(items.len(), 1);
    assert!(items[0].1.contains("HashMap<"));
}

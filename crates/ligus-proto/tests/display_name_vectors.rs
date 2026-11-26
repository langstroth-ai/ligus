//! The shared display-name vectors (review item S3): the iOS app runs the
//! same file through its `NameSanitizer`, so both clean names and flag
//! look-alikes identically.

use ligus_proto::names::{clean_display_name, name_skeleton};

#[test]
fn name_and_skeleton_vectors_agree() {
    let doc: serde_json::Value = serde_json::from_str(include_str!("vectors/display-names.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 30);
    for c in cases {
        let input = c["input"].as_str().unwrap();
        assert_eq!(clean_display_name(input), c["clean"].as_str().unwrap(), "clean: {}", c["note"]);
        assert_eq!(name_skeleton(input), c["skeleton"].as_str().unwrap(), "skeleton: {}", c["note"]);
        // Cleaning is idempotent, and the skeleton only sees the cleaned name.
        let cleaned = clean_display_name(input);
        assert_eq!(clean_display_name(&cleaned), cleaned, "{}", c["note"]);
        assert_eq!(name_skeleton(&cleaned), name_skeleton(input), "{}", c["note"]);
    }
}

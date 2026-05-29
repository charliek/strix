use strix::ui::syntax::{highlight_line, syntax_for};

#[test]
fn rust_line_splits_into_multiple_tokens() {
    let syntax = syntax_for("main.rs");
    let segments = highlight_line(syntax, "let x = 5;");
    assert!(
        segments.len() > 1,
        "expected multiple highlighted tokens, got {segments:?}"
    );
    let joined: String = segments.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(joined, "let x = 5;", "highlighting preserves the text");
}

#[test]
fn unknown_extension_falls_back_to_plain_text() {
    let syntax = syntax_for("notes.unknownext");
    let segments = highlight_line(syntax, "hello world");
    let joined: String = segments.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(joined, "hello world");
}

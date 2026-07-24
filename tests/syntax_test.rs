use strix::ui::syntax::{highlight_line, syntax_for};

#[test]
fn extended_syntax_set_covers_extra_languages() {
    // Exact names pin each path to its intended grammar — a lookup that
    // resolved to some unrelated syntax would otherwise pass unnoticed.
    for (path, name) in [
        ("main.kt", "Kotlin"),
        ("build.gradle.kts", "Kotlin"),
        ("app.ts", "TypeScript"),
        ("app.tsx", "TypeScriptReact"),
        ("main.swift", "Swift"),
        ("Cargo.toml", "TOML"),
        ("Dockerfile", "Dockerfile"),
        ("main.zig", "Zig"),
        ("main.dart", "Dart"),
        ("default.nix", "Nix"),
        ("index.mjs", "JavaScript"),
        ("index.cjs", "JavaScript"),
        ("index.mts", "TypeScript"),
        ("build.zon", "Zig"),
        ("INDEX.MJS", "JavaScript"), // aliases match case-insensitively
        ("page.tmpl", "HTML"),       // template fallback kept from the old set
        ("page.tpl", "HTML"),
    ] {
        let syntax = syntax_for(path);
        assert!(
            syntax.name.to_lowercase().contains(&name.to_lowercase()),
            "expected {path} to resolve to a syntax containing {name:?}, got {:?}",
            syntax.name
        );
    }
}

#[test]
fn previously_supported_extensions_keep_their_syntax() {
    for (path, name) in [
        ("main.rs", "Rust"),
        ("app.py", "Python"),
        ("app.js", "JavaScript"),
    ] {
        let syntax = syntax_for(path);
        assert!(
            syntax.name.to_lowercase().contains(&name.to_lowercase()),
            "expected {path} to still resolve to {name:?}, got {:?}",
            syntax.name
        );
    }
}

#[test]
fn highlights_kotlin_line_with_multiple_tokens() {
    let syntax = syntax_for("main.kt");
    let segments = highlight_line(
        syntax,
        "base16-ocean.dark",
        "fun main(args: Array<String>) {",
    );
    assert!(
        segments.len() > 1,
        "expected multiple highlighted tokens, got {segments:?}"
    );
}

#[test]
fn rust_line_splits_into_multiple_tokens() {
    let syntax = syntax_for("main.rs");
    let segments = highlight_line(syntax, "base16-ocean.dark", "let x = 5;");
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
    assert_eq!(syntax.name, "Plain Text");
    let segments = highlight_line(syntax, "base16-ocean.dark", "hello world");
    let joined: String = segments.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(joined, "hello world");
}

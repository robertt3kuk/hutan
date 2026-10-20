//! Prints the tree of a source snippet with byte ranges: `cargo run -p hutan-syntax --example sexp
//! -- <language> '<source with \n escapes>'` (debugging aid for queries).
use hutan_syntax::{Language, Snapshot, SyntaxNode};

fn dump(node: SyntaxNode<'_>, depth: usize) {
    let range = node.range();
    println!(
        "{}{}{} {}..{}{}",
        "  ".repeat(depth),
        node.kind(),
        if node.is_named() { "" } else { " (token)" },
        range.start,
        range.end,
        if node.is_missing() { " MISSING" } else { "" }
    );
    for child in node.children() {
        dump(child, depth + 1);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(name), Some(source)) = (args.next(), args.next()) else {
        eprintln!("usage: sexp <language> <source>");
        return;
    };
    let Some(language) = Language::from_name(&name) else {
        eprintln!("unknown language {name}");
        return;
    };
    match Snapshot::parse(language, source.replace("\\n", "\n"), 0) {
        Ok(snapshot) => dump(snapshot.root(), 0),
        Err(error) => eprintln!("{error}"),
    }
}

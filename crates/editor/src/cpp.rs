//! C++ highlighting. tree-sitter-cpp's highlight query only has what C++ adds
//! (upstream editors load it on top of C's: "inherits: c"), and GPUI Kit uses
//! it alone, which leaves strings, comments, `#include` and numbers plain. We
//! register `cpp` again with C's query first and C++'s after it (later patterns
//! win), replacing GPUI Kit's entry.

use gpui_kit::component::highlighter::{LanguageConfig, LanguageRegistry};

pub const NAME: &str = "cpp";

fn highlights() -> String {
    format!(
        "{}\n{}",
        tree_sitter_c::HIGHLIGHT_QUERY,
        tree_sitter_cpp::HIGHLIGHT_QUERY
    )
}

pub fn init() {
    LanguageRegistry::singleton().register(
        NAME,
        &LanguageConfig::new(
            NAME,
            tree_sitter_cpp::LANGUAGE.into(),
            vec![],
            &highlights(),
            "",
            "",
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"#include "a.h"
#include <vector>
// a comment
namespace n {
int f() { auto s = "text"; return g(42); }
}
"#;

    fn captures() -> Vec<(String, String)> {
        let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
        let q = highlights();
        let query = tree_sitter::Query::new(&language, &q).expect("query compiles");
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(SAMPLE, None).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut out = Vec::new();
        use tree_sitter::StreamingIterator as _;
        let mut it = cursor.captures(&query, tree.root_node(), SAMPLE.as_bytes());
        while let Some((m, ix)) = it.next() {
            let c = m.captures[*ix];
            let name = query.capture_names()[c.index as usize].to_string();
            out.push((name, SAMPLE[c.node.byte_range()].to_string()));
        }
        out
    }

    #[::core::prelude::v1::test]
    fn c_and_cpp_patterns_both_apply() {
        let caps = captures();
        let has = |n: &str, t: &str| caps.iter().any(|(a, b)| a == n && b == t);
        assert!(has("keyword", "#include"), "{caps:?}");
        assert!(has("string", "\"a.h\""));
        assert!(has("string", "<vector>"));
        assert!(has("comment", "// a comment"));
        assert!(has("string", "\"text\""));
        assert!(has("number", "42"));
        assert!(has("keyword", "namespace"));
        assert!(has("function", "g"));
    }
}

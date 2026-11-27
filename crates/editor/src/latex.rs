//! LaTeX highlighting: GPUI Kit has no LaTeX grammar, so we register one
//! (latex-lsp's tree-sitter-latex, MIT) with GPUI Kit's language registry at
//! startup, with our own highlight query mapped to GPUI Kit's theme captures.

use gpui_kit::component::highlighter::{LanguageConfig, LanguageRegistry};

pub const NAME: &str = "latex";

/// Highlight query. Capture names are GPUI Kit's theme keys (`title`,
/// `keyword`, `function`, `label`, `string.special`, …); later patterns win
/// over earlier ones for the same text, so specific ones come last.
pub const HIGHLIGHTS: &str = r#"
(command_name) @function

[(line_comment) (block_comment) (comment_environment)] @comment

(begin command: _ @keyword name: (curly_group_text (text) @type))
(end command: _ @keyword name: (curly_group_text (text) @type))

(part command: _ @keyword text: (curly_group) @title)
(chapter command: _ @keyword text: (curly_group) @title)
(section command: _ @keyword text: (curly_group) @title)
(subsection command: _ @keyword text: (curly_group) @title)
(subsubsection command: _ @keyword text: (curly_group) @title)
(paragraph command: _ @keyword text: (curly_group) @title)
(subparagraph command: _ @keyword text: (curly_group) @title)
(title_declaration command: _ @keyword text: (curly_group) @title)
(author_declaration command: _ @keyword)
(caption command: _ @keyword)
(enum_item command: _ @keyword)

(label_definition command: _ @keyword name: (curly_group_label) @label)
(label_reference command: _ @keyword names: (curly_group_label_list) @label)
(citation command: _ @keyword keys: (curly_group_text_list) @label)

(class_include command: _ @preproc path: (curly_group_path) @string)
(package_include command: _ @preproc paths: (curly_group_path_list) @string)
(latex_include command: _ @preproc path: (curly_group_path) @string)
(graphics_include command: _ @function path: (curly_group_path) @string)
(hyperlink command: _ @function uri: (curly_group_uri) @link_uri)

(new_command_definition command: _ @keyword declaration: (_) @function)
(theorem_definition command: _ @keyword)

(key_value_pair key: (text) @property)

[(inline_formula) (displayed_equation)] @string.special
(math_environment) @string.special
(source_code) @text.literal

((generic_command
   command: (command_name) @_cmd
   arg: (curly_group) @emphasis.strong)
 (#eq? @_cmd "\\textbf"))
((generic_command
   command: (command_name) @_cmd
   arg: (curly_group) @emphasis)
 (#match? @_cmd "^\\\\(emph|textit)$"))
"#;

/// File extensions that open as LaTeX.
pub fn is_latex_ext(ext: &str) -> bool {
    matches!(ext, "tex" | "sty" | "cls" | "ltx" | "dtx")
}

pub fn init() {
    LanguageRegistry::singleton().register(
        NAME,
        &LanguageConfig::new(
            NAME,
            codebook_tree_sitter_latex::LANGUAGE.into(),
            vec![],
            HIGHLIGHTS,
            "",
            "",
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"\documentclass[midterm]{sparreport}
\usepackage{booktabs}
% a comment
\title{Coherence-preserving Activation Steering}
\section{Introduction}\label{sec:intro}
As shown in \cref{sec:intro} and \cite{smith2024}, $x^2 + y$ is \textbf{bold}.
\begin{equation}
  E = mc^2
\end{equation}
"#;

    fn captures() -> Vec<(String, String)> {
        let language: tree_sitter::Language = codebook_tree_sitter_latex::LANGUAGE.into();
        let query = tree_sitter::Query::new(&language, HIGHLIGHTS).expect("query compiles");
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

    fn has(caps: &[(String, String)], name: &str, text: &str) -> bool {
        caps.iter().any(|(n, t)| n == name && t == text)
    }

    #[test]
    fn query_compiles_and_highlights() {
        let caps = captures();
        assert!(has(&caps, "comment", "% a comment"), "{caps:?}");
        assert!(has(&caps, "title", "{Introduction}"));
        assert!(has(
            &caps,
            "title",
            "{Coherence-preserving Activation Steering}"
        ));
        assert!(has(&caps, "label", "{sec:intro}"));
        assert!(has(&caps, "label", "{smith2024}"));
        assert!(has(&caps, "preproc", "\\usepackage"));
        assert!(has(&caps, "string", "{booktabs}"));
        assert!(has(&caps, "string.special", "$x^2 + y$"));
        assert!(has(&caps, "type", "equation"));
        assert!(has(&caps, "emphasis.strong", "{bold}"));
    }
}

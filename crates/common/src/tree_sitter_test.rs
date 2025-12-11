use tree_sitter::{Parser, Tree};

/// Initialize a tree-sitter parser with JavaScript language support
pub fn create_js_parser() -> Parser {
    let mut parser = Parser::new();
    let language = tree_sitter_javascript::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to set JavaScript language");
    parser
}

/// Parse JavaScript source code and return the syntax tree
pub fn parse_js(source: &str) -> Option<Tree> {
    let mut parser = create_js_parser();
    parser.parse(source, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parser_creation() {
        let parser = create_js_parser();
        // Parser should be created successfully with JavaScript language
        assert!(parser.language().is_some());
    }

    #[test]
    fn test_parse_simple_function() {
        let source = "function add(a, b) { return a + b; }";
        let tree = parse_js(source).expect("Failed to parse JavaScript");

        let root_node = tree.root_node();
        assert_eq!(root_node.kind(), "program");
        assert_eq!(root_node.child_count(), 1);

        // First child should be a function declaration
        let function_node = root_node.child(0).expect("Should have a child");
        assert_eq!(function_node.kind(), "function_declaration");
    }

    #[test]
    fn test_parse_variable_declaration() {
        let source = "const x = 42;";
        let tree = parse_js(source).expect("Failed to parse JavaScript");

        let root_node = tree.root_node();
        assert_eq!(root_node.kind(), "program");

        // First child should be a lexical declaration
        let declaration = root_node.child(0).expect("Should have a child");
        assert_eq!(declaration.kind(), "lexical_declaration");
    }

    #[test]
    fn test_parse_complex_code() {
        let source = r#"
            class Calculator {
                constructor() {
                    this.result = 0;
                }
                
                add(x, y) {
                    return x + y;
                }
            }
            
            const calc = new Calculator();
            console.log(calc.add(5, 3));
        "#;

        let tree = parse_js(source).expect("Failed to parse JavaScript");
        let root_node = tree.root_node();

        assert_eq!(root_node.kind(), "program");
        // Should have at least 3 children: class, const declaration, expression statement
        assert!(root_node.child_count() >= 3);

        // Verify the class declaration
        let class_node = root_node.child(0).expect("Should have first child");
        assert_eq!(class_node.kind(), "class_declaration");
    }

    #[test]
    fn test_parse_arrow_function() {
        let source = "const multiply = (a, b) => a * b;";
        let tree = parse_js(source).expect("Failed to parse JavaScript");

        let root_node = tree.root_node();
        assert_eq!(root_node.kind(), "program");

        let declaration = root_node.child(0).expect("Should have a child");
        assert_eq!(declaration.kind(), "lexical_declaration");
    }

    #[test]
    fn test_parse_with_syntax_error() {
        // This should still produce a tree, but with error nodes
        let source = "function broken( { return x; }";
        let tree = parse_js(source);

        assert!(tree.is_some());
        let tree = tree.unwrap();
        assert!(tree.root_node().has_error());
    }
}

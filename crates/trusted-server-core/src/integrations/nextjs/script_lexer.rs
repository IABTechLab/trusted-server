//! Bounded lexical context for recognizing executable Flight call heads.
//!
//! This is not a JavaScript parser. It masks strings, comments and template text
//! without changing byte offsets, and exposes code inside template expressions.

const MAX_TEMPLATE_DEPTH: usize = 32;

#[derive(Clone, Debug, Default)]
enum Mode {
    #[default]
    Code,
    Slash {
        regex: bool,
    },
    Regex {
        escaped: bool,
        class: bool,
    },
    String {
        quote: char,
        escaped: bool,
    },
    LineComment,
    BlockComment {
        star: bool,
    },
    Template {
        escaped: bool,
        dollar: bool,
    },
    Opaque,
}

#[derive(Clone, Copy, Debug)]
enum BraceKind {
    StatementBlock,
    ExpressionBlock,
    Object,
}

#[derive(Clone, Copy, Debug)]
enum ParenthesisKind {
    Control,
    Expression,
    Function(BraceKind),
}

#[derive(Clone, Copy, Debug)]
struct PendingClass {
    body: BraceKind,
    parenthesis_depth: usize,
    brace_depth: usize,
}

#[derive(Clone, Debug)]
pub(super) struct ScriptLexer {
    mode: Mode,
    template_braces: Vec<usize>,
    expression_start: bool,
    statement_start: bool,
    braces: Vec<BraceKind>,
    word: String,
    word_statement_start: bool,
    word_overflow: bool,
    word_property: bool,
    after_dot: bool,
    control_pending: bool,
    parentheses: Vec<ParenthesisKind>,
    function_pending: Option<BraceKind>,
    body_pending: Option<BraceKind>,
    classes: Vec<PendingClass>,
    operator: Option<(char, bool)>,
}

impl Default for ScriptLexer {
    fn default() -> Self {
        Self {
            mode: Mode::Code,
            template_braces: Vec::new(),
            expression_start: true,
            statement_start: true,
            braces: Vec::new(),
            word: String::new(),
            word_statement_start: true,
            word_overflow: false,
            word_property: false,
            after_dot: false,
            control_pending: false,
            parentheses: Vec::new(),
            function_pending: None,
            body_pending: None,
            classes: Vec::new(),
            operator: None,
        }
    }
}

impl ScriptLexer {
    /// Mask non-code bytes while preserving source offsets and opening quotes.
    /// Opening quotes remain visible so the Flight payload regex can match them.
    pub(super) fn mask(&mut self, source: &str) -> String {
        let mut code = String::with_capacity(source.len());
        for character in source.chars() {
            if self.is_code(character) {
                code.push(character);
            } else {
                // A non-whitespace sentinel prevents matching across inert text.
                code.extend(std::iter::repeat_n('#', character.len_utf8()));
            }
        }
        code
    }

    /// Excessive lexical nesting is unsupported rather than unbounded.
    pub(super) fn is_opaque(&self) -> bool {
        matches!(self.mode, Mode::Opaque)
    }

    fn is_code(&mut self, character: char) -> bool {
        match &mut self.mode {
            Mode::Code => {
                if character.is_alphanumeric() || matches!(character, '_' | '$') {
                    if self.word.is_empty() {
                        self.word_property = self.after_dot;
                        self.word_statement_start = self.statement_start;
                        self.after_dot = false;
                        self.body_pending = None;
                    }
                    self.operator = None;
                    if self.word.len() < 10 && !self.word_overflow {
                        self.word.push(character);
                    } else {
                        self.word_overflow = true;
                    }
                    self.expression_start = false;
                    self.statement_start = false;
                    return true;
                }
                if !self.word.is_empty() {
                    self.expression_start = !self.word_overflow
                        && !self.word_property
                        && matches!(
                            self.word.as_str(),
                            "return"
                                | "throw"
                                | "case"
                                | "delete"
                                | "void"
                                | "typeof"
                                | "new"
                                | "yield"
                                | "await"
                                | "else"
                                | "do"
                        );
                    self.control_pending = !self.word_overflow
                        && !self.word_property
                        && matches!(
                            self.word.as_str(),
                            "if" | "while" | "for" | "with" | "switch" | "catch"
                        );
                    if !self.word_overflow && !self.word_property {
                        if matches!(self.word.as_str(), "function" | "class") {
                            let body = if self.word_statement_start {
                                BraceKind::StatementBlock
                            } else {
                                BraceKind::ExpressionBlock
                            };
                            if self.word == "function" {
                                self.function_pending = Some(body);
                            } else if self.classes.len() == MAX_TEMPLATE_DEPTH {
                                self.mode = Mode::Opaque;
                                return false;
                            } else {
                                self.classes.push(PendingClass {
                                    body,
                                    parenthesis_depth: self.parentheses.len(),
                                    brace_depth: self.braces.len(),
                                });
                            }
                        }
                        self.statement_start = matches!(self.word.as_str(), "else" | "do")
                            || (self.word_statement_start
                                && matches!(self.word.as_str(), "async" | "export" | "default"));
                    }
                    self.word.clear();
                    self.word_overflow = false;
                }
                let prior_operator = self.operator.take();
                let mut pending_body = None;
                if !character.is_whitespace() && character != '/' {
                    self.after_dot = character == '.';
                    pending_body = self.body_pending.take();
                    if !matches!(character, '(' | '*') {
                        self.function_pending = None;
                    }
                }
                match character {
                    '(' => {
                        if self.parentheses.len() == MAX_TEMPLATE_DEPTH {
                            self.mode = Mode::Opaque;
                        } else {
                            let kind = if self.control_pending {
                                ParenthesisKind::Control
                            } else if let Some(kind) = self.function_pending.take() {
                                ParenthesisKind::Function(kind)
                            } else {
                                ParenthesisKind::Expression
                            };
                            self.parentheses.push(kind);
                        }
                        self.control_pending = false;
                        self.expression_start = true;
                        self.statement_start = false;
                    }
                    ')' => {
                        let kind = self.parentheses.pop();
                        self.expression_start = matches!(kind, Some(ParenthesisKind::Control));
                        self.statement_start = self.expression_start;
                        if let Some(ParenthesisKind::Function(kind)) = kind {
                            self.body_pending = Some(kind);
                        }
                    }
                    '=' => {
                        self.operator = Some((character, self.expression_start));
                        self.expression_start = true;
                        self.statement_start = false;
                    }
                    '>' => {
                        if prior_operator.is_some_and(|(operator, _)| operator == '=') {
                            self.body_pending = Some(BraceKind::ExpressionBlock);
                        }
                        self.expression_start = true;
                        self.statement_start = false;
                    }
                    '+' | '-' => {
                        if let Some((operator, prior_expression)) = prior_operator
                            && operator == character
                        {
                            self.expression_start = prior_expression;
                        } else {
                            self.operator = Some((character, self.expression_start));
                            self.expression_start = true;
                        }
                        self.statement_start = false;
                    }
                    '\'' | '"' => {
                        self.statement_start = false;
                        self.mode = Mode::String {
                            quote: character,
                            escaped: false,
                        };
                    }
                    '/' => {
                        self.mode = Mode::Slash {
                            regex: self.expression_start,
                        }
                    }
                    '`' => {
                        self.statement_start = false;
                        if self.template_braces.len() == MAX_TEMPLATE_DEPTH {
                            self.mode = Mode::Opaque;
                        } else {
                            self.template_braces.push(0);
                            self.mode = Mode::Template {
                                escaped: false,
                                dollar: false,
                            };
                        }
                    }
                    '{' => {
                        // Blocks admit a new statement after closing; objects and
                        // function expressions finish a value and admit division.
                        let class_body = self.classes.last().is_some_and(|class| {
                            class.parenthesis_depth == self.parentheses.len()
                                && class.brace_depth == self.braces.len()
                        });
                        // A superclass function can open at the class head's
                        // depth. Its body must not consume the pending class.
                        let kind = if let Some(body) = pending_body {
                            body
                        } else if class_body {
                            self.classes.pop().expect("should have pending class").body
                        } else if self.statement_start || !self.expression_start {
                            BraceKind::StatementBlock
                        } else {
                            BraceKind::Object
                        };
                        if self.braces.len() == MAX_TEMPLATE_DEPTH {
                            self.mode = Mode::Opaque;
                        } else {
                            self.braces.push(kind);
                        }
                        self.expression_start = true;
                        self.statement_start = !matches!(kind, BraceKind::Object);
                        if let Some(braces) = self.template_braces.last_mut() {
                            *braces = braces.saturating_add(1);
                        }
                    }
                    '}' => {
                        if self.template_braces.last() == Some(&0) {
                            self.mode = Mode::Template {
                                escaped: false,
                                dollar: false,
                            };
                        } else {
                            let kind = self.braces.pop();
                            self.expression_start = matches!(kind, Some(BraceKind::StatementBlock));
                            self.statement_start = self.expression_start;
                            if let Some(braces) = self.template_braces.last_mut() {
                                *braces -= 1;
                            }
                        }
                    }
                    ':' => {
                        // A reserved word used as an object key is not a class head.
                        if self.classes.last().is_some_and(|class| {
                            class.parenthesis_depth == self.parentheses.len()
                                && class.brace_depth == self.braces.len()
                        }) {
                            self.classes.pop();
                        }
                        self.expression_start = true;
                        self.statement_start = false;
                    }
                    _ => {
                        if !character.is_whitespace() {
                            self.expression_start = !matches!(character, ')' | ']' | '.');
                            self.statement_start = character == ';';
                        }
                    }
                }
                true
            }
            Mode::Slash { regex } => {
                match character {
                    '/' => self.mode = Mode::LineComment,
                    '*' => self.mode = Mode::BlockComment { star: false },
                    _ if *regex => {
                        self.mode = Mode::Regex {
                            escaped: false,
                            class: false,
                        };
                        return self.is_code(character);
                    }
                    _ => {
                        self.mode = Mode::Code;
                        self.expression_start = true;
                        self.statement_start = false;
                        self.after_dot = false;
                        return self.is_code(character);
                    }
                }
                false
            }
            Mode::Regex { escaped, class } => {
                if *escaped {
                    *escaped = false;
                } else if character == '\\' {
                    *escaped = true;
                } else if character == '[' {
                    *class = true;
                } else if character == ']' {
                    *class = false;
                } else if character == '/' && !*class {
                    self.mode = Mode::Code;
                    self.expression_start = false;
                }
                false
            }
            Mode::String { quote, escaped } => {
                if *escaped {
                    *escaped = false;
                } else if character == '\\' {
                    *escaped = true;
                } else if character == *quote {
                    self.mode = Mode::Code;
                    self.expression_start = false;
                }
                false
            }
            Mode::LineComment => {
                if matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
                    self.mode = Mode::Code;
                }
                false
            }
            Mode::BlockComment { star } => {
                if *star && character == '/' {
                    self.mode = Mode::Code;
                } else {
                    *star = character == '*';
                }
                false
            }
            Mode::Template { escaped, dollar } => {
                if *escaped {
                    *escaped = false;
                    *dollar = false;
                } else if character == '\\' {
                    *escaped = true;
                    *dollar = false;
                } else if character == '`' {
                    self.template_braces.pop();
                    self.mode = Mode::Code;
                    self.expression_start = false;
                } else if *dollar && character == '{' {
                    self.mode = Mode::Code;
                    self.expression_start = true;
                    self.statement_start = false;
                } else {
                    *dollar = character == '$';
                }
                false
            }
            Mode::Opaque => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_inert_text_at_every_fragment_boundary() {
        let source = r#"'é self.__next_f.push('/* self.__next_f.push( */`😀 ${ {nested: `self.__next_f.push(`} } ${self.__next_f.push([1,"data"])} `"#;
        let expected = ScriptLexer::default().mask(source);
        assert_eq!(expected.len(), source.len(), "should preserve byte offsets");
        assert_eq!(expected.matches("self.__next_f.push(").count(), 1);
        for split in (0..=source.len()).filter(|split| source.is_char_boundary(*split)) {
            let mut lexer = ScriptLexer::default();
            let actual = format!(
                "{}{}",
                lexer.mask(&source[..split]),
                lexer.mask(&source[split..])
            );
            assert_eq!(
                actual, expected,
                "should preserve lexical state at split {split}"
            );
        }
    }

    #[test]
    fn brace_context_survives_every_fragment_boundary() {
        for prefix in [
            "{} /self.__next_f.push()/;",
            "if (ready) { work() } /self.__next_f.push()/;",
            "if (ready) { if (nested) { work() } } /self.__next_f.push()/;",
            "function demo() { return 1 } /self.__next_f.push()/;",
            "async function demo() {} /self.__next_f.push()/;",
            "const n = {} / 2;",
            "const n = {nested: {}} / 2;",
            "const n = function() {} / 2;",
            "const n = function named() { return {} } / 2;",
            "const n = (() => {}) / 2;",
            "class Demo { method() { return 1 } } /self.__next_f.push()/;",
            "const n = class { method() { return 1 } } / 2;",
            "const n = class extends mixin({}) {} / 2;",
            "const n = class extends function() {} {} / 2;",
            "const n = class extends function Base() {} {} / 2;",
            "const n = class extends (function() {}) {} / 2;",
            "const n = class extends (class {}) {} / 2;",
            "const n = {class: {}, method() { return 1 }} / 2;",
            "const n = {method() { if (ready) { work() } /self.__next_f.push()/ }} / 2;",
            "const n = `value ${ {} / 2 }`;",
        ] {
            let source = format!("{prefix}self.__next_f.push([1,\"data\"])");
            for split in 0..=source.len() {
                let mut lexer = ScriptLexer::default();
                let code = format!(
                    "{}{}",
                    lexer.mask(&source[..split]),
                    lexer.mask(&source[split..])
                );
                assert_eq!(code.len(), source.len(), "should preserve source offsets");
                assert_eq!(
                    code.matches("self.__next_f.push(").count(),
                    1,
                    "should expose only the executable push: prefix={prefix}, split={split}"
                );
            }
        }
    }

    #[test]
    fn bounds_nested_braces() {
        let mut lexer = ScriptLexer::default();
        lexer.mask(&"{".repeat(MAX_TEMPLATE_DEPTH + 1));
        assert!(lexer.is_opaque(), "should refuse excessive brace nesting");
        assert_eq!(lexer.braces.len(), MAX_TEMPLATE_DEPTH);
        assert!(!lexer.mask("self.__next_f.push(").contains("__next_f"));
    }

    #[test]
    fn bounds_pending_class_heads() {
        let mut lexer = ScriptLexer::default();
        lexer.mask(&"class extends (".repeat(MAX_TEMPLATE_DEPTH + 1));
        assert!(lexer.is_opaque(), "should refuse excessive class nesting");
        assert_eq!(lexer.classes.len(), MAX_TEMPLATE_DEPTH);
        assert!(!lexer.mask("self.__next_f.push(").contains("__next_f"));
    }

    #[test]
    fn bounds_nested_templates() {
        let mut lexer = ScriptLexer::default();
        lexer.mask(&"`${".repeat(MAX_TEMPLATE_DEPTH + 1));
        assert!(lexer.is_opaque(), "should refuse excessive nesting");
        assert_eq!(lexer.template_braces.len(), MAX_TEMPLATE_DEPTH);
        assert!(!lexer.mask("self.__next_f.push(").contains("__next_f"));
    }
}

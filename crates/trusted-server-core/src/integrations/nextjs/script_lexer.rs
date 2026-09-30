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

#[derive(Clone, Debug)]
pub(super) struct ScriptLexer {
    mode: Mode,
    template_braces: Vec<usize>,
    expression_start: bool,
    word: String,
    word_overflow: bool,
    word_property: bool,
    after_dot: bool,
    control_pending: bool,
    parentheses: Vec<bool>,
    operator: Option<(char, bool)>,
}

impl Default for ScriptLexer {
    fn default() -> Self {
        Self {
            mode: Mode::Code,
            template_braces: Vec::new(),
            expression_start: true,
            word: String::new(),
            word_overflow: false,
            word_property: false,
            after_dot: false,
            control_pending: false,
            parentheses: Vec::new(),
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

    /// Excessively nested templates are unsupported rather than unbounded.
    pub(super) fn is_opaque(&self) -> bool {
        matches!(self.mode, Mode::Opaque)
    }

    fn is_code(&mut self, character: char) -> bool {
        match &mut self.mode {
            Mode::Code => {
                if character.is_alphanumeric() || matches!(character, '_' | '$') {
                    if self.word.is_empty() {
                        self.word_property = self.after_dot;
                        self.after_dot = false;
                    }
                    self.operator = None;
                    if self.word.len() < 10 && !self.word_overflow {
                        self.word.push(character);
                    } else {
                        self.word_overflow = true;
                    }
                    self.expression_start = false;
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
                    self.word.clear();
                    self.word_overflow = false;
                }
                let prior_operator = self.operator.take();
                if !character.is_whitespace() && character != '/' {
                    self.after_dot = character == '.';
                }
                match character {
                    '(' => {
                        if self.parentheses.len() == MAX_TEMPLATE_DEPTH {
                            self.mode = Mode::Opaque;
                        } else {
                            self.parentheses.push(self.control_pending);
                        }
                        self.control_pending = false;
                        self.expression_start = true;
                    }
                    ')' => self.expression_start = self.parentheses.pop().unwrap_or(false),
                    '+' | '-' => {
                        if let Some((operator, prior_expression)) = prior_operator
                            && operator == character
                        {
                            self.expression_start = prior_expression;
                        } else {
                            self.operator = Some((character, self.expression_start));
                            self.expression_start = true;
                        }
                    }
                    '\'' | '"' => {
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
                        self.expression_start = true;
                        if let Some(braces) = self.template_braces.last_mut() {
                            *braces = braces.saturating_add(1);
                        }
                    }
                    '}' => {
                        if let Some(braces) = self.template_braces.last_mut() {
                            if *braces == 0 {
                                self.mode = Mode::Template {
                                    escaped: false,
                                    dollar: false,
                                };
                            } else {
                                *braces -= 1;
                            }
                        }
                    }
                    _ => {
                        if !character.is_whitespace() {
                            self.expression_start = !matches!(character, ')' | ']' | '.');
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
    fn bounds_nested_templates() {
        let mut lexer = ScriptLexer::default();
        lexer.mask(&"`${".repeat(MAX_TEMPLATE_DEPTH + 1));
        assert!(lexer.is_opaque(), "should refuse excessive nesting");
        assert_eq!(lexer.template_braces.len(), MAX_TEMPLATE_DEPTH);
        assert!(!lexer.mask("self.__next_f.push(").contains("__next_f"));
    }
}

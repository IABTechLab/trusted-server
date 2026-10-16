//! Script text a middleware writes, kept inside its `<script>` element.
//!
//! A browser decodes no entity inside a script, so text written there cannot
//! be escaped as markup. The element ends at the first `</script` followed by
//! whitespace, `/` or `>`, in any letter case, and after `<!--` a `<script`
//! followed by one of those stops the element's own end tag from closing it.
//! [`ScriptTextGuard`] reads the text written into a script as a browser's
//! tokenizer does, following the script data states of the HTML standard.
//! Where text a middleware changed would do either, the `<` that begins the
//! sequence is written as `\u003c`, which a JavaScript string, template
//! literal or regular expression and a JSON string all read as `<`.

use std::mem;

/// The tag name both sequences spell, matched in any letter case.
const NAME: &[u8] = b"script";

/// How many letters of [`NAME`] a whole name matches.
const WHOLE: u8 = 6;

/// What `<` is written as where it would let the text out of its script.
const ESCAPED_LESS_THAN: &str = "\\u003c";

/// Where a browser's tokenizer is in the text of a script. A number counts
/// the letters of `script` matched so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum State {
    #[default]
    Data,
    LessThan,
    EscapeStart,
    EscapeStartDash,
    EndTag(u8),
    /// After `<!--`, where `</script` still ends the element.
    Escaped,
    EscapedDash,
    EscapedDashDash,
    EscapedLessThan,
    EscapedEndTag(u8),
    DoubleEscapeStart(u8),
    /// After `<!--` and `<script`, where `</script` no longer ends the
    /// element.
    DoubleEscaped,
    DoubleEscapedDash,
    DoubleEscapedDashDash,
    DoubleEscapedLessThan,
    DoubleEscapeEnd(u8),
}

/// What one character of the text does.
enum Step {
    /// Moves to a state, with the character read.
    Next(State),
    /// Moves to a state, which reads the same character again.
    Again(State),
    /// Ends a sequence that lets the text out of its script, unless its `<`
    /// is escaped, leading to the first state when it is and the second
    /// when it is not.
    Escape(State, State),
}

impl State {
    /// Whether the state is inside a sequence whose `<` may yet have to be
    /// escaped.
    fn is_open(self) -> bool {
        matches!(
            self,
            Self::LessThan
                | Self::EndTag(_)
                | Self::EscapedLessThan
                | Self::EscapedEndTag(_)
                | Self::DoubleEscapeStart(_)
        )
    }

    fn step(self, byte: u8) -> Step {
        match self {
            Self::Data if byte == b'<' => Step::Next(Self::LessThan),
            Self::Data => Step::Next(Self::Data),
            Self::LessThan => match byte {
                b'/' => Step::Next(Self::EndTag(0)),
                b'!' => Step::Next(Self::EscapeStart),
                _ => Step::Again(Self::Data),
            },
            Self::EscapeStart if byte == b'-' => Step::Next(Self::EscapeStartDash),
            Self::EscapeStartDash if byte == b'-' => Step::Next(Self::EscapedDashDash),
            Self::EscapeStart | Self::EscapeStartDash => Step::Again(Self::Data),
            // `</script` would end the element.
            Self::EndTag(WHOLE) if ends_name(byte) => Step::Escape(Self::Data, Self::Data),
            Self::EndTag(matched) => name(matched, byte, Self::EndTag, Self::Data),
            Self::Escaped | Self::EscapedDash | Self::EscapedDashDash => {
                Step::Next(self.escaped(byte))
            }
            Self::EscapedLessThan => match byte {
                b'/' => Step::Next(Self::EscapedEndTag(0)),
                _ if byte.is_ascii_alphabetic() => Step::Again(Self::DoubleEscapeStart(0)),
                _ => Step::Again(Self::Escaped),
            },
            Self::EscapedEndTag(WHOLE) if ends_name(byte) => {
                Step::Escape(Self::Escaped, Self::Escaped)
            }
            Self::EscapedEndTag(matched) => name(matched, byte, Self::EscapedEndTag, Self::Escaped),
            // `<script` inside `<!--` would keep the element's own end tag
            // from closing it.
            Self::DoubleEscapeStart(WHOLE) if ends_name(byte) => {
                Step::Escape(Self::Escaped, Self::DoubleEscaped)
            }
            Self::DoubleEscapeStart(matched) => {
                name(matched, byte, Self::DoubleEscapeStart, Self::Escaped)
            }
            Self::DoubleEscaped | Self::DoubleEscapedDash | Self::DoubleEscapedDashDash => {
                Step::Next(self.double_escaped(byte))
            }
            Self::DoubleEscapedLessThan if byte == b'/' => Step::Next(Self::DoubleEscapeEnd(0)),
            Self::DoubleEscapedLessThan => Step::Again(Self::DoubleEscaped),
            Self::DoubleEscapeEnd(WHOLE) if ends_name(byte) => Step::Next(Self::Escaped),
            Self::DoubleEscapeEnd(matched) => {
                name(matched, byte, Self::DoubleEscapeEnd, Self::DoubleEscaped)
            }
        }
    }

    /// The state after `byte` in one of the states after `<!--`.
    fn escaped(self, byte: u8) -> Self {
        match (self, byte) {
            (_, b'<') => Self::EscapedLessThan,
            (Self::Escaped, b'-') => Self::EscapedDash,
            (Self::EscapedDash | Self::EscapedDashDash, b'-') => Self::EscapedDashDash,
            (Self::EscapedDashDash, b'>') => Self::Data,
            _ => Self::Escaped,
        }
    }

    /// The state after `byte` in one of the states after `<!--` and
    /// `<script`.
    fn double_escaped(self, byte: u8) -> Self {
        match (self, byte) {
            (_, b'<') => Self::DoubleEscapedLessThan,
            (Self::DoubleEscaped, b'-') => Self::DoubleEscapedDash,
            (Self::DoubleEscapedDash | Self::DoubleEscapedDashDash, b'-') => {
                Self::DoubleEscapedDashDash
            }
            (Self::DoubleEscapedDashDash, b'>') => Self::Data,
            _ => Self::DoubleEscaped,
        }
    }
}

/// Whether `byte` ends a tag name, as whitespace, `/` or `>` do.
fn ends_name(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | b'\x0C' | b'\r' | b' ' | b'/' | b'>')
}

/// Reads `byte` in a state that has matched `matched` letters of `script`.
/// A letter that continues `script` moves to `reading`, and a name that can
/// no longer be `script` returns to `other`.
fn name(matched: u8, byte: u8, reading: fn(u8) -> State, other: State) -> Step {
    if !byte.is_ascii_alphabetic() {
        return Step::Again(other);
    }
    match NAME.get(usize::from(matched)) {
        Some(&letter) if byte.to_ascii_lowercase() == letter => Step::Next(reading(matched + 1)),
        _ => Step::Next(other),
    }
}

/// Follows the text written into one script after another, a chunk at a
/// time, and says what to write so that text a middleware changed stays
/// inside its script.
///
/// Text the origin sent is written as it is, because the parser already
/// read all of it as the script's text.
#[derive(Debug, Default)]
pub(super) struct ScriptTextGuard {
    /// Where the tokenizer is after the text written so far.
    state: State,
    /// The end of the last chunk, held back from it because it begins a
    /// sequence that only the next chunk shows the end of.
    held: String,
    /// Whether a middleware changed a chunk of this script so far.
    changed: bool,
}

impl ScriptTextGuard {
    /// What to write in place of one chunk of a script's text, being
    /// `written`, which the handlers left there, after anything held back
    /// from the chunk before. `changed` says whether a handler changed this
    /// chunk, and `is_last` whether it is the last of the script. `None`
    /// leaves the chunk as the handlers left it.
    pub(super) fn write(&mut self, written: &str, changed: bool, is_last: bool) -> Option<String> {
        self.changed |= changed;
        if self.held.is_empty() && self.state == State::Data && !written.contains('<') {
            self.finish(is_last);
            return None;
        }

        let text = format!("{}{written}", mem::take(&mut self.held));
        let bytes = text.as_bytes();
        let mut state = self.state;
        // The `<` that began the sequence being read, and the state before it.
        let mut open: Option<(usize, State)> = None;
        let mut escapes: Vec<usize> = Vec::new();
        let mut index = 0;
        while let Some(&byte) = bytes.get(index) {
            let before = state;
            let read = match state.step(byte) {
                Step::Next(next) => {
                    state = next;
                    true
                }
                Step::Again(next) => {
                    state = next;
                    false
                }
                Step::Escape(escaped, as_written) => {
                    match open {
                        Some((at, _)) if self.changed => {
                            escapes.push(at);
                            state = escaped;
                        }
                        _ => state = as_written,
                    }
                    true
                }
            };
            if !state.is_open() {
                open = None;
            } else if !before.is_open() {
                open = Some((index, before));
            }
            if read {
                index += 1;
            }
        }

        let end = match open {
            Some((at, before)) if !is_last => {
                self.state = before;
                at
            }
            _ => {
                self.state = state;
                text.len()
            }
        };
        self.held = text[end..].to_owned();
        self.finish(is_last);
        if escapes.is_empty() && text.len() == written.len() && end == text.len() {
            return None;
        }

        let mut safe = String::with_capacity(end + escapes.len() * ESCAPED_LESS_THAN.len());
        let mut from = 0;
        for at in escapes {
            safe.push_str(&text[from..at]);
            safe.push_str(ESCAPED_LESS_THAN);
            from = at + 1;
        }
        safe.push_str(&text[from..end]);
        Some(safe)
    }

    /// Starts afresh for the next script once the last chunk of this one is
    /// written.
    fn finish(&mut self, is_last: bool) {
        if is_last {
            *self = Self::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ScriptTextGuard;

    /// What `guard` writes for a script whose chunks are `chunks`, each
    /// changed by a middleware when `changed`.
    fn written(chunks: &[&str], changed: bool) -> String {
        let mut guard = ScriptTextGuard::default();
        let mut page = String::new();
        for (index, chunk) in chunks.iter().enumerate() {
            let is_last = index + 1 == chunks.len();
            match guard.write(chunk, changed, is_last) {
                Some(safe) => page.push_str(&safe),
                None => page.push_str(chunk),
            }
        }
        page
    }

    #[test]
    fn a_closing_tag_a_middleware_writes_is_escaped_in_any_letter_case() {
        for (text, expected) in [
            ("a('</script>')", "a('\\u003c/script>')"),
            ("a('</SCRIPT >')", "a('\\u003c/SCRIPT >')"),
            ("a('</Script/')", "a('\\u003c/Script/')"),
            ("a('</script\n')", "a('\\u003c/script\n')"),
            ("a('<!--</script>')", "a('<!--\\u003c/script>')"),
        ] {
            assert_eq!(written(&[text], true), expected, "should escape `{text}`");
        }
    }

    #[test]
    fn what_cannot_end_the_script_is_written_as_it_is() {
        for text in [
            "if (a < b && c > d) {}",
            "a('</scripts>')",
            "a('</scrip>')",
            "a('<script>')",
            "a('<!--')",
            "<!-- legacy(); //-->",
            "a('</script')",
        ] {
            assert_eq!(
                written(&[text], true),
                text,
                "should leave `{text}` as it is"
            );
        }
    }

    #[test]
    fn a_script_tag_inside_a_comment_opening_a_middleware_writes_is_escaped() {
        assert_eq!(
            written(&["a('<!--<script>')"], true),
            "a('<!--\\u003cscript>')",
            "should keep the script's own end tag closing it"
        );
        assert_eq!(
            written(&["a('<!--<script>'); b('</script>')"], true),
            "a('<!--\\u003cscript>'); b('\\u003c/script>')",
            "should escape the closing tag that the escaped tag leaves able to end the script"
        );
    }

    #[test]
    fn a_sequence_split_across_chunks_is_escaped_where_it_begins() {
        let chunks = ["a('<", "/scr", "ipt", ">');"];
        assert_eq!(written(&chunks, true), "a('\\u003c/script>');");

        let chunks = ["a('", "<", "/", "s", "c", "r", "i", "p", "t", " ", "');"];
        assert_eq!(written(&chunks, true), "a('\\u003c/script ');");
    }

    #[test]
    fn text_the_origin_sent_is_written_as_it_is() {
        let legacy = "<!-- document.write('<script src=\"a.js\"></script>'); //-->";
        assert_eq!(written(&[legacy], false), legacy);
        let chunks = [
            "<!-- document.write('<scr",
            "ipt src=\"a.js\"></scr",
            "ipt>'); //-->",
        ];
        assert_eq!(written(&chunks, false), chunks.concat());
    }
}

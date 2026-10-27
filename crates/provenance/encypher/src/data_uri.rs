//! `data:` URIs carrying media: parsing, exact decoding and lossless
//! re-encoding in the URI's own encoding.
//!
//! A URI is rewritten as `data:` (as written), its header text between the
//! scheme and the comma (as written), and the signed bytes encoded the way
//! the original payload was: base64, padded only if the original was, or
//! percent-encoding with a set that is safe in an HTML attribute (quoted or
//! not), a `srcset` candidate and a CSS `url()`. Decoding the new URI gives
//! back exactly the signed bytes.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use crate::media_api::canonical_media_type;

/// Forgiving base64, as browsers decode a `data:` URI: padding optional and
/// trailing bits ignored. ASCII whitespace is removed before decoding.
const FORGIVING: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// One `data:` URI whose media type is media the module signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DataUri<'a> {
    /// The scheme as written, `data:` in any case.
    scheme: &'a str,
    /// Everything between the scheme and the comma, as written.
    header: &'a str,
    /// The payload after the comma.
    payload: &'a str,
    base64: bool,
}

/// Why a media `data:` URI cannot be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Malformed;

/// Whether `media_type` is media: an image, audio, video or font.
pub(crate) fn is_media_type(media_type: &str) -> bool {
    ["image/", "audio/", "video/", "font/"]
        .iter()
        .any(|prefix| media_type.starts_with(prefix) && media_type.len() > prefix.len())
}

impl<'a> DataUri<'a> {
    /// The `data:` URI `value` is, when it declares media. Any other value,
    /// or a `data:` URI of another type, is `None`.
    pub(crate) fn parse(value: &'a str) -> Option<Self> {
        let scheme = value.get(..5)?;
        if !scheme.eq_ignore_ascii_case("data:") {
            return None;
        }
        let rest = value.get(5..)?;
        let comma = rest.find(',')?;
        let header = rest.get(..comma)?;
        let payload = rest.get(comma + 1..)?;
        let uri = Self {
            scheme,
            header,
            payload,
            base64: header
                .rsplit(';')
                .next()
                .is_some_and(|last| last.trim().eq_ignore_ascii_case("base64"))
                && header.contains(';'),
        };
        is_media_type(&uri.media_type()).then_some(uri)
    }

    /// The bare, lower-case media type the URI declares.
    pub(crate) fn media_type(&self) -> String {
        canonical_media_type(self.header.split(';').next().unwrap_or_default().trim())
    }

    /// The declared type with its parameters, as written, without `;base64`,
    /// sent as the source's type hint.
    pub(crate) fn declared_type(&self) -> &'a str {
        if self.base64 {
            self.header
                .rsplit_once(';')
                .map_or(self.header, |(declared, _)| declared)
        } else {
            self.header
        }
    }

    /// The exact bytes the URI carries.
    ///
    /// # Errors
    ///
    /// Returns [`Malformed`] for base64 a browser would refuse.
    pub(crate) fn decode(&self) -> Result<Vec<u8>, Malformed> {
        if self.base64 {
            let compact: Vec<u8> = self
                .payload
                .bytes()
                .filter(|byte| !byte.is_ascii_whitespace())
                .collect();
            return FORGIVING.decode(compact).map_err(|_| Malformed);
        }
        Ok(percent_decode(self.payload.as_bytes()))
    }

    /// The URI for `bytes`, with this URI's scheme and header text, encoded
    /// as this URI's payload was.
    pub(crate) fn encode(&self, bytes: &[u8]) -> String {
        let mut out = String::with_capacity(
            self.scheme.len() + self.header.len() + 1 + bytes.len().saturating_mul(3),
        );
        out.push_str(self.scheme);
        out.push_str(self.header);
        out.push(',');
        if self.base64 {
            let encoded = STANDARD.encode(bytes);
            if self.payload.trim_end().ends_with('=') || self.unpadded_length_is_aligned() {
                out.push_str(&encoded);
            } else {
                out.push_str(encoded.trim_end_matches('='));
            }
        } else {
            percent_encode_into(&mut out, bytes);
        }
        out
    }

    /// Whether the original payload needed no padding, so writing padding
    /// keeps its form.
    fn unpadded_length_is_aligned(&self) -> bool {
        self.payload
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .count()
            % 4
            == 0
    }
}

/// Decodes `%XX` escapes. A `%` without two hex digits stays as written, as
/// browsers read a `data:` URI.
fn percent_decode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut index = 0;
    while let Some(&byte) = input.get(index) {
        if byte == b'%'
            && let (Some(high), Some(low)) = (
                input.get(index + 1).and_then(|digit| hex_value(*digit)),
                input.get(index + 2).and_then(|digit| hex_value(*digit)),
            )
        {
            out.push((high << 4) | low);
            index += 3;
            continue;
        }
        out.push(byte);
        index += 1;
    }
    out
}

fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

/// Whether `byte` is written as itself in a percent-encoded payload. Quotes,
/// `&`, `<`, `>`, `=`, whitespace, parentheses, `\`, `,`, `#` and `%` are
/// always escaped, so the payload is safe in every context the module
/// rewrites and a `srcset` candidate never ends in a comma.
fn is_literal(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'*'
                | b'+'
                | b';'
                | b':'
                | b'@'
                | b'/'
                | b'?'
        )
}

fn percent_encode_into(out: &mut String, bytes: &[u8]) {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in bytes {
        if is_literal(byte) {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(DIGITS[usize::from(byte >> 4)]));
            out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_media_data_uris_are_parsed() {
        assert!(DataUri::parse("data:image/png;base64,iVBORw0KGgo=").is_some());
        assert!(DataUri::parse("DATA:Image/SVG+xml;charset=utf-8,%3Csvg%3E").is_some());
        assert!(DataUri::parse("data:font/woff2;base64,d09GMg==").is_some());
        for value in [
            "data:text/html,<p>x</p>",
            "data:,plain",
            "data:application/json;base64,e30=",
            "https://cdn.example.com/a.png",
            "data:image/png;base64",
        ] {
            assert_eq!(DataUri::parse(value), None, "should not sign {value}");
        }
    }

    #[test]
    fn base64_round_trips_exact_bytes_and_keeps_header_and_padding_style() {
        let uri = DataUri::parse("Data:image/png;name=a.png;BASE64,AAEC/w==")
            .expect("should parse a media data URI");
        assert_eq!(uri.media_type(), "image/png");
        assert_eq!(uri.declared_type(), "image/png;name=a.png");
        assert_eq!(
            uri.decode().expect("should decode"),
            vec![0, 1, 2, 255],
            "should decode the exact bytes"
        );
        let signed: Vec<u8> = (0..=255).collect();
        let encoded = uri.encode(&signed);
        assert!(
            encoded.starts_with("Data:image/png;name=a.png;BASE64,"),
            "should keep the scheme and header as written: {encoded}"
        );
        let reparsed = DataUri::parse(&encoded).expect("should reparse");
        assert_eq!(
            reparsed.decode().expect("should decode"),
            signed,
            "should round-trip"
        );

        let unpadded = DataUri::parse("data:image/gif;base64,AAEC/w").expect("should parse");
        let encoded = unpadded.encode(&[1, 2]);
        assert!(!encoded.ends_with('='), "should keep an unpadded form");
        assert_eq!(
            DataUri::parse(&encoded)
                .expect("should reparse")
                .decode()
                .expect("should decode"),
            vec![1, 2]
        );
    }

    #[test]
    fn wrapped_base64_is_decoded_as_browsers_do() {
        let uri = DataUri::parse("data:image/png;base64,AAEC\n /w==").expect("should parse");
        assert_eq!(uri.decode().expect("should decode"), vec![0, 1, 2, 255]);
        let bad = DataUri::parse("data:image/png;base64,A*==").expect("should parse");
        assert_eq!(bad.decode(), Err(Malformed), "should refuse invalid base64");
    }

    #[test]
    fn percent_encoded_svg_round_trips_and_is_context_safe() {
        let original = "data:image/svg+xml;charset=utf-8,%3Csvg xmlns='http://www.w3.org/2000/svg'%3E%3C/svg%3E";
        let uri = DataUri::parse(original).expect("should parse");
        let decoded = uri.decode().expect("should decode");
        assert_eq!(
            decoded,
            b"<svg xmlns='http://www.w3.org/2000/svg'></svg>".to_vec(),
            "should decode escapes and keep literal bytes"
        );
        let signed = b"<svg a=\"1\" b='2'>(x), #&\\ \n100%</svg>\xff";
        let encoded = uri.encode(signed);
        assert!(
            !encoded[original.find(',').expect("comma") + 1..]
                .bytes()
                .any(|byte| matches!(
                    byte,
                    b'"' | b'\''
                        | b'<'
                        | b'>'
                        | b'&'
                        | b'='
                        | b'('
                        | b')'
                        | b'\\'
                        | b','
                        | b'#'
                        | b' '
                        | b'\n'
                )),
            "should escape every context-sensitive byte: {encoded}"
        );
        assert_eq!(
            DataUri::parse(&encoded)
                .expect("should reparse")
                .decode()
                .expect("should decode"),
            signed.to_vec(),
            "should round-trip exactly"
        );
    }
}

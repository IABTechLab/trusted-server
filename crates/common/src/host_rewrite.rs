/// Rewrite bare host occurrences (e.g. `origin.example.com/news`) only when the match is a full
/// hostname token, not part of a larger hostname like `cdn.origin.example.com`.
///
/// This is used by both HTML (`__next_f` payloads) and Flight (`text/x-component`) rewriting to
/// avoid corrupting unrelated hostnames.
pub(crate) fn rewrite_bare_host_at_boundaries(
    text: &str,
    origin_host: &str,
    request_host: &str,
) -> Option<String> {
    if origin_host.is_empty() || request_host.is_empty() || !text.contains(origin_host) {
        return None;
    }

    fn is_host_char(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':')
    }

    let origin_len = origin_host.len();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut search = 0;
    let mut replaced_any = false;

    while let Some(rel) = text[search..].find(origin_host) {
        let pos = search + rel;
        let end = pos + origin_len;

        let before_ok = pos == 0 || !is_host_char(bytes[pos - 1]);
        let after_ok = end == bytes.len() || !is_host_char(bytes[end]);

        if before_ok && after_ok {
            out.push_str(&text[search..pos]);
            out.push_str(request_host);
            replaced_any = true;
            search = end;
        } else {
            out.push_str(&text[search..pos + 1]);
            search = pos + 1;
        }
    }

    if !replaced_any {
        return None;
    }

    out.push_str(&text[search..]);
    Some(out)
}

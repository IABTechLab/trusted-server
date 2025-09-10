use fastly::http::{header, StatusCode};
use fastly::{Request, Response};
use sha2::{Digest, Sha256};

/// Build a static text response with strong ETag and standard caching headers.
/// Handles If-None-Match to return 304 when appropriate.
pub fn serve_static_with_etag(body: &str, req: &Request, content_type: &str) -> Response {
    // Compute ETag for conditional caching
    let hash = Sha256::digest(body.as_bytes());
    let etag = format!("\"sha256-{}\"", hex::encode(hash));

    // If-None-Match handling for 304 responses
    if let Some(if_none_match) = req
        .get_header(header::IF_NONE_MATCH)
        .and_then(|h| h.to_str().ok())
    {
        if if_none_match == etag {
            return Response::from_status(StatusCode::NOT_MODIFIED)
                .with_header(header::ETAG, &etag)
                .with_header(
                    header::CACHE_CONTROL,
                    "public, max-age=300, s-maxage=300, stale-while-revalidate=60, stale-if-error=86400",
                )
                .with_header("surrogate-control", "max-age=300")
                .with_header(header::VARY, "Accept-Encoding");
        }
    }

    Response::from_status(StatusCode::OK)
        .with_header(header::CONTENT_TYPE, content_type)
        .with_header(
            header::CACHE_CONTROL,
            "public, max-age=300, s-maxage=300, stale-while-revalidate=60, stale-if-error=86400",
        )
        .with_header("surrogate-control", "max-age=300")
        .with_header(header::ETAG, &etag)
        .with_header(header::VARY, "Accept-Encoding")
        .with_body(body)
}

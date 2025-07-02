use crate::constants::HEADER_X_REQUEST_ID;
use crate::http_wrapper::RequestWrapper;
use fastly::Response;
use uuid::Uuid;

/// Generate a new request ID
pub fn generate_request_id() -> String {
    Uuid::new_v4().to_string()
}

/// Get request ID from headers or generate a new one
pub fn get_or_generate_request_id<T: RequestWrapper>(req: &T) -> String {
    req.get_header(HEADER_X_REQUEST_ID)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(generate_request_id)
}

/// Add request ID to response headers
pub fn add_request_id_to_response(response: &mut Response, request_id: &str) {
    response.set_header(HEADER_X_REQUEST_ID, request_id);
}

/// Log with request ID context
#[macro_export]
macro_rules! log_with_request_id {
    ($level:ident, $request_id:expr, $($arg:tt)+) => {
        log::$level!("[{}] {}", $request_id, format!($($arg)+));
    };
}

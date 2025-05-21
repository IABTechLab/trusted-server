use http::header::HeaderName;

pub const HEADER_SYNTHETIC_FRESH: HeaderName = HeaderName::from_static("x-synthetic-fresh");
pub const HEADER_SYNTHETIC_PUB_USER_ID: HeaderName = HeaderName::from_static("x-pub-user-id");
pub const HEADER_SYNTHETIC_TRUSTED_SERVER: HeaderName =
    HeaderName::from_static("x-synthetic-trusted-server");
pub const HEADER_X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
pub const HEADER_X_SUBJECT_ID: HeaderName = HeaderName::from_static("x-subject-id");

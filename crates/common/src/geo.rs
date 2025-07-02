use fastly::Response;

use crate::constants::{
    HEADER_X_GEO_CITY, HEADER_X_GEO_CONTINENT, HEADER_X_GEO_COORDINATES, HEADER_X_GEO_COUNTRY,
    HEADER_X_GEO_INFO_AVAILABLE, HEADER_X_GEO_METRO_CODE,
};
use crate::http_wrapper::RequestWrapper;

/// Copy all geo headers from request to response
pub fn copy_geo_headers<T: RequestWrapper>(req: &T, response: &mut Response) {
    let geo_headers = &[
        HEADER_X_GEO_CITY,
        HEADER_X_GEO_COUNTRY,
        HEADER_X_GEO_CONTINENT,
        HEADER_X_GEO_COORDINATES,
        HEADER_X_GEO_METRO_CODE,
        HEADER_X_GEO_INFO_AVAILABLE,
    ];

    for header_name in geo_headers {
        if let Some(value) = req.get_header(header_name.clone()) {
            response.set_header(header_name, value);
        }
    }
}

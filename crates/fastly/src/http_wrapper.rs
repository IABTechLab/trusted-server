use fastly::Request as FastlyRequest;
use http::header::{HeaderName, HeaderValue};
use http::Method;
use std::net::IpAddr;

use trusted_server_common::http_wrapper::RequestWrapper;

#[derive(Debug)]
pub struct FastlyRequestWrapper {
    request: FastlyRequest,
}

impl FastlyRequestWrapper {
    pub fn new(request: FastlyRequest) -> Self {
        FastlyRequestWrapper { request }
    }
}

impl RequestWrapper for FastlyRequestWrapper {
    #[inline(always)]
    fn get_client_ip_addr(&self) -> Option<IpAddr> {
        self.request.get_client_ip_addr()
    }

    #[inline(always)]
    fn get_header(&self, name: HeaderName) -> Option<&HeaderValue> {
        self.request.get_header(name)
    }

    #[inline(always)]
    fn get_headers(&self) -> impl Iterator<Item = (&HeaderName, &HeaderValue)> {
        self.request.get_headers()
    }

    #[inline(always)]
    fn get_method(&self) -> &Method {
        self.request.get_method()
    }

    #[inline(always)]
    fn get_path(&self) -> &str {
        self.request.get_path()
    }

    #[inline(always)]
    fn set_header(&mut self, name: HeaderName, value: HeaderValue) {
        self.request.set_header(name, value)
    }
}

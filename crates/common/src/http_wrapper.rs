use http::header::{HeaderName, HeaderValue};
use http::Method;
use std::net::IpAddr;

pub trait RequestWrapper {
    fn get_client_ip_addr(&self) -> Option<IpAddr>;

    fn get_header(&self, name: HeaderName) -> Option<&HeaderValue>;

    fn get_headers(&self) -> impl Iterator<Item = (&HeaderName, &HeaderValue)>;

    fn get_method(&self) -> &Method;

    fn get_path(&self) -> &str;

    #[allow(clippy::wrong_self_convention)]
    fn into_body_bytes(&mut self) -> Vec<u8>;

    fn set_header(&mut self, name: HeaderName, value: HeaderValue);

    fn set_header_str(&mut self, name: HeaderName, value: &str) {
        self.set_header(name, HeaderValue::from_str(value).unwrap())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use http::request::Builder;

    #[derive(Debug)]
    pub struct HttpRequestWrapper {
        builder: Builder,
    }

    impl HttpRequestWrapper {
        pub fn new(builder: Builder) -> Self {
            HttpRequestWrapper { builder }
        }
    }

    impl RequestWrapper for HttpRequestWrapper {
        #[inline(always)]
        fn get_client_ip_addr(&self) -> Option<IpAddr> {
            Some(IpAddr::from([127, 0, 0, 1])) // Placeholder for testing
        }

        #[inline(always)]
        fn get_header(&self, name: HeaderName) -> Option<&HeaderValue> {
            self.builder.headers_ref().unwrap().get(name)
        }

        #[inline(always)]
        fn get_headers(&self) -> impl Iterator<Item = (&HeaderName, &HeaderValue)> {
            self.builder.headers_ref().unwrap().iter()
        }

        #[inline(always)]
        fn get_method(&self) -> &Method {
            self.builder.method_ref().unwrap()
        }

        #[inline(always)]
        fn get_path(&self) -> &str {
            self.builder.uri_ref().unwrap().path()
        }

        fn into_body_bytes(&mut self) -> Vec<u8> {
            // TODO: Implement the actual value extraction logic
            vec![]
        }

        #[inline(always)]
        fn set_header(&mut self, _name: HeaderName, _value: HeaderValue) {
            // TODO: Implement the actual header setting logic
            // self.builder = self.builder.header(name, value);
        }
    }
}

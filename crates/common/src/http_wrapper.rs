use http::Method;
use http::header::{HeaderName, HeaderValue};
use std::net::IpAddr;

pub trait RequestWrapper {
    fn get_client_ip_addr(&self) -> Option<IpAddr>;

    fn get_header(
        &self,
        name: HeaderName,
    ) -> Option<&HeaderValue>;

    fn get_headers(&self) -> impl Iterator<Item = (&HeaderName, &HeaderValue)>;

    fn get_method(&self) -> &Method;

    fn get_path(&self) -> &str;

    fn set_header(&mut self, name: HeaderName, value: HeaderValue);
}

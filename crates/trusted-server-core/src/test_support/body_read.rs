//! Request bodies that drive each failure [`read_request_body`] maps, for reader route tests.
//!
//! [`read_request_body`]: crate::http_util::read_request_body

use std::cell::Cell;
use std::rc::Rc;
use std::task::Poll;

use bytes::Bytes;
use edgezero_core::body::Body;
use edgezero_core::error::EdgeError;
use error_stack::Report;
use http::{Request, Response, StatusCode, header};

use crate::error::{IntoHttpResponse as _, TrustedServerError};

/// One body-read failure that a reader route must answer with a fixed status.
pub(crate) struct BodyReadCase {
    name: &'static str,
    content_length: Option<String>,
    body: Body,
    expected: StatusCode,
    polled: Option<Rc<Cell<bool>>>,
}

impl BodyReadCase {
    /// Sets this case's body and declared length on `builder` and builds the request.
    ///
    /// # Panics
    ///
    /// Panics when `builder` cannot build a request.
    pub(crate) fn into_request(
        self,
        builder: http::request::Builder,
    ) -> (Request<Body>, BodyReadExpectation) {
        let builder = match &self.content_length {
            Some(declared) => builder.header(header::CONTENT_LENGTH, declared.as_str()),
            None => builder,
        };
        let request = builder
            .body(self.body)
            .expect("should build the body-read case request");
        let expectation = BodyReadExpectation {
            name: self.name,
            expected: self.expected,
            polled: self.polled,
        };
        (request, expectation)
    }
}

/// What a reader route must answer for one [`BodyReadCase`].
pub(crate) struct BodyReadExpectation {
    name: &'static str,
    expected: StatusCode,
    polled: Option<Rc<Cell<bool>>>,
}

impl BodyReadExpectation {
    /// Names the case, for assertion messages that callers add.
    pub(crate) fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the status this case must answer with.
    pub(crate) fn expected(&self) -> StatusCode {
        self.expected
    }

    /// Asserts that the route answered `status`, and never polled a declared overflow.
    ///
    /// # Panics
    ///
    /// Panics when the status differs or the declared-overflow body was polled.
    pub(crate) fn assert_status(&self, route: &str, status: StatusCode) {
        assert_eq!(
            status, self.expected,
            "{route} should answer the {} case with {}",
            self.name, self.expected
        );
        if let Some(polled) = &self.polled {
            assert!(
                !polled.get(),
                "{route} should reject the {} case without polling the body",
                self.name
            );
        }
    }

    /// Asserts on the status of a handler outcome, whether a response or an error.
    ///
    /// # Panics
    ///
    /// Panics as [`Self::assert_status`] does.
    pub(crate) fn assert_outcome(
        &self,
        route: &str,
        outcome: &Result<Response<Body>, Report<TrustedServerError>>,
    ) {
        self.assert_status(route, outcome_status(outcome));
    }
}

/// Returns the status a handler outcome answers with, as an adapter renders it.
pub(crate) fn outcome_status(
    outcome: &Result<Response<Body>, Report<TrustedServerError>>,
) -> StatusCode {
    match outcome {
        Ok(response) => response.status(),
        Err(report) => report.current_context().status_code(),
    }
}

/// Returns the five body-read failures every reader route maps, for a reader capped at `max`.
///
/// The cases are a declared length over `max` (413, never polled), a streamed
/// body that crosses `max` (413), a read past the inbound deadline (408), a
/// client-caused stream error (400), and a host read error (502).
pub(crate) fn body_read_failure_cases(max: usize) -> Vec<BodyReadCase> {
    let polled = Rc::new(Cell::new(false));
    let poll_flag = Rc::clone(&polled);
    let unpolled = Body::from_stream(futures::stream::poll_fn(move |_| {
        poll_flag.set(true);
        Poll::Ready(None::<Result<Bytes, EdgeError>>)
    }));

    vec![
        BodyReadCase {
            name: "declared over cap",
            content_length: Some((max + 1).to_string()),
            body: unpolled,
            expected: StatusCode::PAYLOAD_TOO_LARGE,
            polled: Some(polled),
        },
        BodyReadCase {
            name: "streamed over cap",
            content_length: None,
            body: streamed_over(max),
            expected: StatusCode::PAYLOAD_TOO_LARGE,
            polled: None,
        },
        failing_case(
            "read deadline",
            EdgeError::request_timeout("expired"),
            StatusCode::REQUEST_TIMEOUT,
        ),
        failing_case(
            "client stream error",
            EdgeError::bad_request("malformed chunk framing"),
            StatusCode::BAD_REQUEST,
        ),
        failing_case(
            "host read error",
            EdgeError::bad_gateway("host read failed"),
            StatusCode::BAD_GATEWAY,
        ),
    ]
}

/// Returns a streamed body of `max` bytes followed by one more, with no declared length.
pub(crate) fn streamed_over(max: usize) -> Body {
    let chunks: [Result<Bytes, EdgeError>; 2] = [
        Ok(Bytes::from(vec![b'x'; max])),
        Ok(Bytes::from_static(b"x")),
    ];
    Body::from_stream(futures::stream::iter(chunks))
}

fn failing_case(name: &'static str, error: EdgeError, expected: StatusCode) -> BodyReadCase {
    BodyReadCase {
        name,
        content_length: None,
        body: Body::from_stream(futures::stream::iter([Err::<Bytes, _>(error)])),
        expected,
        polled: None,
    }
}

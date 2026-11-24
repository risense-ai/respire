use axum::{
    body::Body,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug)]
pub(in crate::runtime_http) struct Problem {
    status: StatusCode,
    bearer_challenge: bool,
    code: &'static str,
    title: &'static str,
}

pub(in crate::runtime_http) type V1Result<T> = Result<T, Problem>;

impl Problem {
    pub(in crate::runtime_http) fn new(
        status: StatusCode,
        code: &'static str,
        title: &'static str,
    ) -> Self {
        Self {
            status,
            bearer_challenge: false,
            code,
            title,
        }
    }
}

impl Problem {
    pub(in crate::runtime_http) fn bearer_challenge(mut self) -> Self {
        self.bearer_challenge = true;
        self
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        // Only fixed descriptions enter this transport. Internal paths, identifiers
        // and credential-bearing nested errors are never reflected to callers.
        let body = json!({
            "type": "about:blank",
            "title": self.title,
            "status": self.status.as_u16(),
            "detail": self.title,
            "code": self.code,
            "retryable": self.status == StatusCode::SERVICE_UNAVAILABLE
        });
        let mut response = (
            self.status,
            [("content-type", "application/problem+json")],
            Body::from(body.to_string()),
        )
            .into_response();
        if self.bearer_challenge {
            response.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Bearer"),
            );
        }
        response
    }
}

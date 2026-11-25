//! Shared registration metadata used by the live router and parity tests.
use axum::{
    handler::Handler,
    routing::{on, MethodFilter},
    Router,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Verb {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Verb {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }

    const fn filter(self) -> MethodFilter {
        match self {
            Self::Get => MethodFilter::GET,
            Self::Post => MethodFilter::POST,
            Self::Put => MethodFilter::PUT,
            Self::Patch => MethodFilter::PATCH,
            Self::Delete => MethodFilter::DELETE,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RouteSpec {
    pub(super) method: Verb,
    pub(super) path: &'static str,
    pub(super) operation_id: &'static str,
}

impl RouteSpec {
    pub(super) const fn new(method: Verb, path: &'static str, operation_id: &'static str) -> Self {
        Self {
            method,
            path,
            operation_id,
        }
    }
}

pub(super) fn bind<S, T, H>(router: Router<S>, spec: RouteSpec, handler: H) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    T: 'static,
    H: Handler<T, S>,
{
    #[cfg(test)]
    super::route_parity::record(spec);
    router.route(spec.path, on(spec.method.filter(), handler))
}

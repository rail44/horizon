//! The proxy's `HttpHandler`: allow/deny by CONNECT target host (or, for
//! plain HTTP, the request's own host), never terminating TLS
//! (`docs/agent-approval-design.md`: "No MITM by default... allow/deny by
//! CONNECT target host (SNI/authority), TLS not terminated").
//!
//! `should_intercept_connect`/`should_intercept_tls` are both hardcoded to
//! `false`: hudsucker's own CONNECT handling (see `proxy::internal::
//! InternalProxy::process_connect`) rewinds whatever bytes it peeked at for
//! protocol sniffing back onto the stream before falling through to a
//! plain `TcpStream::connect` + `copy_bidirectional` tunnel when
//! interception is declined, so a refused-interception CONNECT is a
//! byte-for-byte transparent tunnel, never a decrypted one.

use std::sync::Arc;

use hudsucker::hyper::{Method, Request, Response, StatusCode};
use hudsucker::{Body, HttpContext, HttpHandler, RequestOrResponse};

use crate::allowlist::Allowlist;

/// A response header naming this proxy's own refusal, distinct from a 403
/// the destination server itself might send -- the "expose that refusal in
/// a way a caller could later classify as a boundary crossing" hook the
/// design doc calls for (`docs/agent-approval-design.md`). The judge/policy
/// leg that consumes this is future work; for now it's a stable, documented
/// marker.
pub const DENIAL_HEADER: &str = "x-horizon-sandbox-proxy-denial";
pub const DENIAL_REASON_NOT_ALLOWLISTED: &str = "host-not-allowlisted";

#[derive(Clone)]
pub(crate) struct AllowlistHandler {
    allowlist: Arc<Allowlist>,
}

impl AllowlistHandler {
    pub(crate) fn new(allowlist: Allowlist) -> Self {
        Self {
            allowlist: Arc::new(allowlist),
        }
    }
}

impl HttpHandler for AllowlistHandler {
    async fn handle_request(
        &mut self,
        _ctx: &HttpContext,
        req: Request<Body>,
    ) -> RequestOrResponse {
        match target_host(&req) {
            Some(host) if self.allowlist.is_allowed(&host) => req.into(),
            Some(host) => forbidden(&host).into(),
            None => forbidden("(no host in request)").into(),
        }
    }

    async fn should_intercept_connect(&mut self, _ctx: &HttpContext, _req: &Request<Body>) -> bool {
        false
    }

    async fn should_intercept_tls(
        &mut self,
        _ctx: &HttpContext,
        _client_hello: hudsucker::rustls::server::ClientHello<'_>,
    ) -> bool {
        false
    }
}

/// The host a request is destined for: the CONNECT authority for a CONNECT
/// request (the HTTPS-tunnel path), else the absolute-form URI's host (the
/// plain-HTTP forward-proxy path), else the `Host` header as a last resort.
fn target_host(req: &Request<Body>) -> Option<String> {
    if req.method() == Method::CONNECT {
        return req.uri().authority().map(|a| a.host().to_string());
    }
    if let Some(host) = req.uri().host() {
        return Some(host.to_string());
    }
    req.headers()
        .get(hudsucker::hyper::header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(':').next().unwrap_or(value).to_string())
}

fn forbidden(host: &str) -> Response<Body> {
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header(DENIAL_HEADER, DENIAL_REASON_NOT_ALLOWLISTED)
        .body(Body::from(format!(
            "horizon-sandbox-proxy: host not allowlisted: {host}"
        )))
        .expect("a hardcoded, static-shaped response should always build")
}

/// A `CertificateAuthority` that must never actually be invoked: this
/// handler's `should_intercept_tls` always returns `false`, so hudsucker
/// never reaches the `gen_server_config` call this would otherwise service
/// (see `proxy::internal::InternalProxy::process_connect`). Exists only
/// because `ProxyBuilder::with_ca` requires *some* implementation
/// structurally, even when TLS interception is permanently declined.
#[derive(Clone, Copy)]
pub(crate) struct NeverInterceptCa;

impl hudsucker::certificate_authority::CertificateAuthority for NeverInterceptCa {
    async fn gen_server_config(
        &self,
        _authority: &hudsucker::hyper::http::uri::Authority,
    ) -> Arc<hudsucker::rustls::ServerConfig> {
        unreachable!(
            "AllowlistHandler::should_intercept_tls always returns false, so hudsucker \
             should never call gen_server_config"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hudsucker::hyper::Uri;

    fn connect_request(authority: &str) -> Request<Body> {
        Request::builder()
            .method(Method::CONNECT)
            .uri(authority)
            .body(Body::empty())
            .unwrap()
    }

    fn absolute_get_request(uri: &str) -> Request<Body> {
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    #[test]
    fn connect_authority_is_the_target_host() {
        let req = connect_request("example.com:443");
        assert_eq!(target_host(&req).as_deref(), Some("example.com"));
    }

    #[test]
    fn absolute_form_uri_host_is_the_target_host() {
        let req = absolute_get_request("http://example.com/path");
        assert_eq!(target_host(&req).as_deref(), Some("example.com"));
    }

    #[test]
    fn falls_back_to_the_host_header_for_origin_form_requests() {
        let mut req = Request::builder()
            .method(Method::GET)
            .uri(Uri::from_static("/path"))
            .body(Body::empty())
            .unwrap();
        req.headers_mut().insert(
            hudsucker::hyper::header::HOST,
            "example.com:8080".parse().unwrap(),
        );
        assert_eq!(target_host(&req).as_deref(), Some("example.com"));
    }

    #[test]
    fn no_host_anywhere_is_none() {
        let req = Request::builder()
            .method(Method::GET)
            .uri(Uri::from_static("/path"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(target_host(&req), None);
    }

    // `AllowlistHandler::handle_request`/`should_intercept_connect` are not
    // unit-tested directly here: hudsucker's `HttpContext` is
    // `#[non_exhaustive]` with no public constructor, so it can only ever
    // be built by hudsucker itself from a real accepted connection. The
    // handler's allow/deny behavior (including the refusal shape --
    // `DENIAL_HEADER`/`StatusCode::FORBIDDEN`) and its never-intercept
    // posture are instead proven against a real running `AllowlistProxy`
    // in `crate::tests` (wire-level, over a plain TCP client).
}

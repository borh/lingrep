//! Security headers applied to every response.
//!
//! The CSP is the interesting one:
//!
//! - `script-src 'self' 'wasm-unsafe-eval'` — every script is a vendored,
//!   self-hosted asset (no inline scripts, no CDN), but the tree-sitter lingrep patterns
//!   grammar runs as WebAssembly in the page, which requires the wasm escape
//!   hatch under CSP 3.
//! - `style-src 'self' 'unsafe-inline'` — capture-lane grids in
//!   `macros.html` compute `grid-column` spans in the template and emit them
//!   as inline `style` attributes, so inline styles stay allowed. Static
//!   inline styles were moved into `app.css`; the remaining ones are
//!   data-driven.
//! - `connect-src 'self'` — the SSE progress stream and the fetch-based
//!   lingrep patterns check/format endpoints are same-origin.
//! - `img-src 'self' data:` — the favicon is a `data:` URL.
//!
//! `base-uri 'none'`, `object-src 'none'`, `frame-ancestors 'none'`, and
//! `form-action 'self'` close off the usual injection escape hatches for an
//! app that renders researcher-supplied corpus text.
//!
//! - `Permissions-Policy` — denies geolocation/camera/microphone/payment, none
//!   of which this app uses.
//! - `Cross-Origin-Opener-Policy: same-origin` — isolates the browsing context;
//!   safe because nothing here opens cross-origin popups.

use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};

const CSP: &str = concat!(
    "default-src 'self'; ",
    "script-src 'self' 'wasm-unsafe-eval'; ",
    "style-src 'self' 'unsafe-inline'; ",
    "img-src 'self' data:; ",
    "connect-src 'self'; ",
    "font-src 'self'; ",
    "object-src 'none'; ",
    "base-uri 'none'; ",
    "form-action 'self'; ",
    "frame-ancestors 'none'"
);

/// Deny powerful browser features this app never uses; single-origin isolation.
const PERMISSIONS_POLICY: &str = "geolocation=(), camera=(), microphone=(), payment=()";

pub async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        axum::http::header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        axum::http::header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static(PERMISSIONS_POLICY),
    );
    headers.insert(
        axum::http::header::HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("same-origin"),
    );
    response
}

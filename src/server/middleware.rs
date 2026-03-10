// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use axum::http::{HeaderValue, Response};

pub fn apply_security_headers<B>(mut response: Response<B>) -> Response<B> {
    let headers = response.headers_mut();
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "x-xss-protection",
        HeaderValue::from_static("1; mode=block"),
    );
    headers.insert(
        "strict-transport-security",
        HeaderValue::from_static("max-age=63072000; includeSubDomains"),
    );
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'"),
    );
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "x-agpl-source",
        HeaderValue::from_static("https://github.com/memoryOSScom/memoryoss"),
    );
    headers.insert("x-license", HeaderValue::from_static("AGPL-3.0-only"));
    response
}

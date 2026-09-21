//! HTTP request helpers for Maypop API calls.

use anyhow::Result;
use reqwest::{header, RequestBuilder};
use serde::Serialize;

/// Attach a JSON body with an explicit byte length.
pub(crate) fn json<T: Serialize>(request: RequestBuilder, value: &T) -> Result<RequestBuilder> {
    let body = serde_json::to_vec(value)?;
    Ok(request
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, body.len())
        .body(body))
}

/// Mark a bodyless request with an explicit zero byte length.
pub(crate) fn empty(request: RequestBuilder) -> RequestBuilder {
    request.header(header::CONTENT_LENGTH, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Client;
    use serde_json::json;

    #[test]
    fn json_body_has_an_exact_content_length() {
        let request = json(
            Client::new().post("https://example.com"),
            &json!({ "name": "Brew" }),
        )
        .unwrap()
        .build()
        .unwrap();
        let body = request.body().and_then(reqwest::Body::as_bytes).unwrap();

        assert_eq!(
            request.headers()[header::CONTENT_LENGTH],
            body.len().to_string()
        );
        assert_eq!(request.headers()[header::CONTENT_TYPE], "application/json");
    }

    #[test]
    fn empty_body_has_a_zero_content_length() {
        let request = empty(Client::new().post("https://example.com"))
            .build()
            .unwrap();

        assert_eq!(request.headers()[header::CONTENT_LENGTH], "0");
    }
}

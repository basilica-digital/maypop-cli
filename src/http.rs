//! HTTP request helpers for Maypop API calls.

use anyhow::{anyhow, Result};
use reqwest::{header, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};

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

/// Describe a failed Maypop response. A plan refusal says what to buy and where.
pub(crate) fn failure(status: StatusCode, body: &str) -> anyhow::Error {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ApiError {
        code: String,
        billing_url: Option<String>,
    }
    match serde_json::from_str::<ApiError>(body) {
        Ok(error) if error.code == "plan_required" => {
            let upgrade = error
                .billing_url
                .map(|url| format!(" Upgrade at {url}"))
                .unwrap_or_default();
            anyhow!("The Maypop CLI needs a Pro plan.{upgrade}")
        }
        _ => anyhow!("Maypop returned {status}: {body}"),
    }
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

//! A thin client of the control plane API (D85).

use control_plane_common::Error;
use reqwest::Method;
use serde_json::Value;

pub struct Client {
    http: reqwest::Client,
    url: String,
    token: String,
}

impl Client {
    pub fn new(url: &str, token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            url: url.trim_end_matches('/').to_owned(),
            token,
        }
    }

    /// Sends one request. Returns the JSON reply, `None` when the reply is empty.
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>, String> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("cannot reach {}: {e}", self.url))?;
        let status = response.status();
        let text = response.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            // The API answers with an `Error`; anything else came from somewhere else.
            return Err(match serde_json::from_str::<Error>(&text) {
                Ok(error) => error.to_string(),
                Err(_) => format!("{status}: {text}"),
            });
        }
        if text.is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("invalid reply: {e}"))
    }
}

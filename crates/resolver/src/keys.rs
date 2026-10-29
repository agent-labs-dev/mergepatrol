//! Checks a workspace's API key with its provider before it is stored, so a typo fails in the
//! dashboard and not in a rebase.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use mergepatrol_core::Provider;
use serde_json::Value;

#[derive(Debug, PartialEq, Eq)]
pub enum KeyCheck {
    Valid,
    Invalid(String),
}

/// Asks `provider` whether `key` works, with the cheapest call it has.
pub async fn check_api_key(provider: Provider, key: &str) -> Result<KeyCheck> {
    let http = reqwest::Client::new();
    let req = match provider {
        Provider::Anthropic => http
            .get("https://api.anthropic.com/v1/models?limit=1")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        Provider::OpenAi => http
            .get("https://api.openai.com/v1/models")
            .bearer_auth(key),
        Provider::OpenRouter => http
            .get("https://openrouter.ai/api/v1/key")
            .bearer_auth(key),
    };
    let name = provider.name();
    let res = req
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .with_context(|| format!("could not reach the {name} API"))?;
    let status = res.status();
    if status.is_success() {
        return Ok(KeyCheck::Valid);
    }
    let body: Value = res.json().await.unwrap_or_default();
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or("rejected")
        .to_owned();
    match status.as_u16() {
        401 | 403 => Ok(KeyCheck::Invalid(message)),
        _ => bail!("the {name} API answered {status}: {message}"),
    }
}

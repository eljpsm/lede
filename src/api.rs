//! The one HTTP call. The request body is the lowest common denominator of
//! the chat-completions wire format, model and messages only, so any
//! OpenAI-compatible provider accepts it; response parsing ignores every
//! field it does not need for the same reason.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::auth::Authorization;

/// One message in the conversation. `role` is `&'static str` because lede
/// only ever sends its own three wire strings: "system", "user", and
/// "assistant" (the last one when the retry replays the model's reply).
#[derive(Serialize)]
pub(crate) struct ChatMessage {
    pub role: &'static str,
    pub content: String,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

/// The error shape most providers return. Parsed best-effort: a body that
/// is something else falls back to its first line.
#[derive(Deserialize)]
struct ApiError {
    error: ApiErrorBody,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    message: String,
}

/// One chat-completions call, returning the assistant's text. Takes the
/// full message list rather than a fixed system-and-user pair so the
/// subject-shortening retry in `app` can extend the conversation.
pub(crate) fn chat(
    base_url: &str,
    model: &str,
    authorization: &Authorization,
    messages: &[ChatMessage],
) -> anyhow::Result<String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    // status-as-error is off because a non-2xx body carries the provider's
    // own error message, which beats a bare status code on stderr. The
    // timeout is generous for slow local models but still ends a hung
    // provider; without one, `git commit -m "$(lede generate)"` blocks forever.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(120)))
        .build()
        .into();

    let mut request = agent.post(&url);
    if let Authorization::Bearer(key) = authorization {
        request = request.header("Authorization", &format!("Bearer {key}"));
    }
    let mut response = request
        .send_json(ChatRequest { model, messages })
        .with_context(|| format!("request to {url} failed"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        let detail = serde_json::from_str::<ApiError>(&body)
            .map(|parsed| parsed.error.message)
            .unwrap_or_else(|_| body.lines().next().unwrap_or_default().trim().to_owned());
        anyhow::bail!("API error (HTTP {}): {}", status.as_u16(), detail);
    }

    let parsed: ChatResponse = response
        .body_mut()
        .read_json()
        .context("failed to parse API response")?;
    let content = parsed
        .choices
        .into_iter()
        .next()
        .context("API returned no choices")?
        .message
        .content
        .unwrap_or_default();
    if content.trim().is_empty() {
        anyhow::bail!("API returned an empty message");
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_request_serializes_to_the_wire_format() {
        let messages = [
            ChatMessage {
                role: "system",
                content: "be brief".into(),
            },
            ChatMessage {
                role: "user",
                content: "the diff".into(),
            },
        ];
        let value = serde_json::to_value(ChatRequest {
            model: "gpt-5.4-nano",
            messages: &messages,
        })
        .unwrap();
        assert_eq!(
            value,
            json!({
                "model": "gpt-5.4-nano",
                "messages": [
                    {"role": "system", "content": "be brief"},
                    {"role": "user", "content": "the diff"},
                ],
            })
        );
    }

    // serde ignores unknown fields by default; this pins that no one adds
    // deny_unknown_fields here. Providers decorate responses differently
    // (usage, system_fingerprint, ...), and rejecting them would break
    // every provider except the one tested against.
    #[test]
    fn a_response_parses_and_ignores_unknown_fields() {
        let body = r#"{
            "id": "chatcmpl-1", "object": "chat.completion", "usage": {"total_tokens": 9},
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "Add feature"}}]
        }"#;
        let parsed: ChatResponse = serde_json::from_str(body).unwrap();
        assert_eq!(
            parsed.choices[0].message.content.as_deref(),
            Some("Add feature")
        );
    }

    #[test]
    fn null_content_parses_as_none() {
        let body = r#"{"choices": [{"message": {"role": "assistant", "content": null}}]}"#;
        let parsed: ChatResponse = serde_json::from_str(body).unwrap();
        assert!(parsed.choices[0].message.content.is_none());
    }

    #[test]
    fn an_empty_choice_list_parses() {
        let parsed: ChatResponse = serde_json::from_str(r#"{"choices": []}"#).unwrap();
        assert!(parsed.choices.is_empty());
    }

    #[test]
    fn the_provider_error_shape_parses() {
        let body = r#"{"error": {"message": "Incorrect API key provided", "type": "auth"}}"#;
        let parsed: ApiError = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.error.message, "Incorrect API key provided");
    }
}

use super::auth::{AuthKind, TwitchAuth};
use anyhow::{Context, Result, bail};
use reqwest::{Client as HttpClient, Method};
use serde_json::{Value, json};

pub struct TwitchClient {
    http: HttpClient,
    auth: TwitchAuth,
    control_token: String,
    message_token: Option<String>,
    pub channels: Vec<String>,
}
impl TwitchClient {
    pub async fn connect(config: &crate::config::TwitchConfig, cloud: &crate::config::CloudConfig) -> Result<Option<Self>> {
        let mut auth = TwitchAuth::new(config, cloud);
        auth.load().await?;
        let Some(control) = auth.token(AuthKind::Control).await? else {
            crate::internal::log::warn("Twitch control auth is missing; skipping Twitch initialization");
            return Ok(None);
        };
        auth.require_client_id()?;
        let http = HttpClient::builder().timeout(std::time::Duration::from_secs(15)).build()?;
        let mut instance = Self {
            http, auth, control_token: control.access_token,
            message_token: None, channels: config.channels.clone(),
        };
        instance.validate_token(&instance.control_token).await.context("Twitch control auth validation failed")?;
        crate::internal::log::success("Twitch control auth validated");
        if let Some(message) = instance.auth.token(AuthKind::Message).await? {
            match instance.validate_token(&message.access_token).await {
                Ok(_) => {
                    instance.message_token = Some(message.access_token);
                    crate::internal::log::success("Twitch message auth validated");
                }
                Err(e) => crate::internal::log::warn(&format!("Twitch message auth unavailable; using control auth: {e}")),
            }
        }
        // TODO: Chat listener and command registration (TwitchCommands.ts).
        // TODO: EventSub WebSocket and subscription/event registrations (Client.ts).
        // TODO: Reconnect generation, retry/backoff, permission refresh and event lifecycle.
        Ok(Some(instance))
    }
    pub async fn validate_token(&self, token: &str) -> Result<Value> {
        let response = self.http.get("https://id.twitch.tv/oauth2/validate")
            .header("Authorization", format!("OAuth {token}"))
            .send().await?;
        let code = response.status();
        let payload: Value = response.json().await?;
        if !code.is_success() { bail!("Twitch token validation HTTP {code}: {}", payload.get("message").and_then(Value::as_str).unwrap_or("invalid token")); }
        if let Some(actual_id) = payload.get("client_id").and_then(Value::as_str) {
            if actual_id != self.auth.client_id { bail!("Twitch token belongs to a different client ID") }
        }
        Ok(payload)
    }
    async fn api(&self, method: Method, path: &str, query: &[(&str, &str)], body: Option<Value>, message_auth: bool) -> Result<Value> {
        let token = if message_auth { self.message_token.as_deref().unwrap_or(&self.control_token) } else { &self.control_token };
        let mut request = self.http.request(method, format!("https://api.twitch.tv/helix/{path}"))
            .header("Client-Id", &self.auth.client_id)
            .bearer_auth(token).query(query);
        if let Some(body) = body { request = request.json(&body); }
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        let payload: Value = if text.is_empty() { Value::Null } else { serde_json::from_str(&text).with_context(|| format!("invalid Twitch Helix response: {text}"))? };
        if !status.is_success() { bail!("Twitch Helix HTTP {status}: {}", payload.get("message").and_then(Value::as_str).unwrap_or(&text)); }
        Ok(payload)
    }
    pub async fn user_by_login(&self, login: &str) -> Result<Option<Value>> {
        let value = self.api(Method::GET, "users", &[("login", login)], None, false).await?;
        Ok(value["data"].as_array().and_then(|a| a.first()).cloned())
    }
    pub async fn user_by_id(&self, id: &str) -> Result<Option<Value>> {
        let value = self.api(Method::GET, "users", &[("id", id)], None, false).await?;
        Ok(value["data"].as_array().and_then(|a| a.first()).cloned())
    }
    pub async fn stream_by_user_id(&self, id: &str) -> Result<Option<Value>> {
        let value = self.api(Method::GET, "streams", &[("user_id", id)], None, false).await?;
        Ok(value["data"].as_array().and_then(|a| a.first()).cloned())
    }
    pub async fn send_chat_message(&self, broadcaster_id: &str, message: &str, reply_to: Option<&str>) -> Result<Value> {
        let sender_id = self.validate_token(self.message_token.as_deref().unwrap_or(&self.control_token)).await?
            .get("user_id").and_then(Value::as_str).context("Twitch token has no user_id")?.to_string();
        let mut body = json!({"broadcaster_id": broadcaster_id, "sender_id": sender_id, "message": message});
        if let Some(reply) = reply_to { body["reply_parent_message_id"] = json!(reply); }
        // Attempt message auth first; if the API rejects it, retry under control auth.
        if self.message_token.is_some() {
            match self.api(Method::POST, "chat/messages", &[], Some(body.clone()), true).await {
                Ok(v) => return Ok(v),
                Err(e) => crate::internal::log::warn(&format!("Twitch message auth send failed, retrying control auth: {e}")),
            }
            let control_id = self.validate_token(&self.control_token).await?
                .get("user_id").and_then(Value::as_str).context("Control token has no user_id")?.to_string();
            body["sender_id"] = json!(control_id);
        }
        self.api(Method::POST, "chat/messages", &[], Some(body), false).await
    }
    // TODO: Announcements, whispers, bans, timeouts, chat settings, VIP/moderator operations.
    // TODO: Twitch data polling, chatter lists, follower/subscription info, user permissions.
    // TODO: Chat commands, channel point and other EventSub handlers are intentionally not active.
}

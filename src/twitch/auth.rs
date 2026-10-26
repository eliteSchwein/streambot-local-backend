use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use reqwest::Client as HttpClient;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind { Control, Message }
impl AuthKind { pub fn as_str(self) -> &'static str { match self { Self::Control => "control", Self::Message => "message" } } }

// Preserve camelCase fields saved by Twurple and tolerate OAuth snake_case aliases.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoredToken {
    #[serde(rename = "accessToken", alias = "access_token", default)] pub access_token: String,
    #[serde(rename = "refreshToken", alias = "refresh_token", default)] pub refresh_token: String,
    #[serde(rename = "expiresIn", alias = "expires_in", default)] pub expires_in: Option<u64>,
    #[serde(rename = "obtainmentTimestamp", alias = "obtainment_timestamp", default)] pub obtainment_timestamp: Option<u64>,
    #[serde(default)] pub scope: Vec<String>,
    #[serde(rename = "userId", alias = "user_id", default)] pub user_id: Option<String>,
    #[serde(default)] pub login: Option<String>,
    #[serde(rename = "clientId", alias = "client_id", default)] pub client_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TwitchAuth {
    path: PathBuf,
    pub client_id: String,
    pub client_secret: String,
    pub cloud_url: String,
}

impl TwitchAuth {
    pub fn new(config: &crate::config::TwitchConfig, cloud: &crate::config::CloudConfig) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        Self { path: home.join(".config/streambot/integrations.json"), client_id: config.client_id.clone(), client_secret: config.client_secret.clone(), cloud_url: cloud.url.trim_end_matches('/').to_owned() }
    }
    pub async fn load(&mut self) -> Result<()> {
        let contents = match tokio::fs::read_to_string(&self.path).await {
            Ok(v) => v,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let doc: Value = serde_json::from_str(&contents).with_context(|| format!("invalid {}", self.path.display()))?;
        let twitch = &doc["twitch"];
        // Prefer explicitly configured credentials, otherwise use the client ID
        // embedded in cloud-issued OAuth tokens (Twurple integrations.json format).
        fn first_nonempty<'a>(values: &[Option<&'a str>]) -> Option<&'a str> {
            values
                .iter()
                .flatten()
                .copied()
                .find(|value| !value.trim().is_empty())
        }
        if let Some(id) = first_nonempty(&[
            twitch["client_id"].as_str(),
            twitch["clientId"].as_str(),
            twitch["control"]["clientId"].as_str(),
            twitch["control"]["client_id"].as_str(),
            twitch["message"]["clientId"].as_str(),
            twitch["message"]["client_id"].as_str(),
        ]) {
            // An explicit [twitch] .env client_id wins; otherwise the cloud token supplies it.
            if self.client_id.trim().is_empty() {
                self.client_id = id.to_owned();
            }
        }
        if let Some(secret) = twitch["client_secret"].as_str().or_else(|| twitch["clientSecret"].as_str()) { if !secret.is_empty() { self.client_secret = secret.into(); } }
        Ok(())
    }
    pub async fn token(&self, kind: AuthKind) -> Result<Option<StoredToken>> {
        let contents = match tokio::fs::read_to_string(&self.path).await {
            Ok(v) => v,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let doc: Value = serde_json::from_str(&contents)?;
        let Some(raw) = doc.get("twitch").and_then(|v| v.get(kind.as_str())) else { return Ok(None) };
        let token: StoredToken = serde_json::from_value(raw.clone())?;
        Ok((!token.access_token.is_empty()).then_some(token))
    }
    pub fn require_client_id(&self) -> Result<&str> {
        if self.client_id.is_empty() { bail!("Twitch client ID is missing (integrations.json or [twitch] in .env)") }
        Ok(&self.client_id)
    }
    /// The cloud broker initiates Twitch OAuth; this backend never receives an OAuth client secret.
    pub fn cloud_auth_url(&self, kind: AuthKind, callback_url: &str) -> Result<String> {
        let path = match kind { AuthKind::Control => "/auth/bot", AuthKind::Message => "/auth/message-bot" };
        let mut url = reqwest::Url::parse(&format!("{}{path}", self.cloud_url))?;
        url.query_pairs_mut().append_pair("return_url", callback_url);
        Ok(url.to_string())
    }

    /// Redeem a short-lived exchange code issued by StreamDing Cloud.
    /// The broker is authoritative; the locally stored token is just the resulting credential.
    pub async fn exchange_code(&self, kind: AuthKind, code: &str) -> Result<StoredToken> {
        let url = format!("{}/api/v1/twitch/exchange", self.cloud_url);
        let response = HttpClient::builder().timeout(std::time::Duration::from_secs(15)).build()?
            .post(url).json(&json!({"code": code})).send().await?;
        let status = response.status();
        let body: Value = response.json().await?;
        if !status.is_success() { bail!("Cloud exchange failed (HTTP {status})") }
        let root = body.get("data").unwrap_or(&body);
        let raw = root.get("token").or_else(|| root.get("twitch")).unwrap_or(root);
        let mut token: StoredToken = serde_json::from_value(raw.clone()).context("invalid cloud Twitch token")?;
        if token.access_token.is_empty() { bail!("cloud exchange returned no access token") }
        if token.client_id.as_deref().unwrap_or("").is_empty() {
            token.client_id = root.get("clientId").or_else(||root.get("client_id")).and_then(Value::as_str).map(str::to_owned);
        }
        if token.client_id.as_deref().unwrap_or("").is_empty() { token.client_id = Some(self.require_client_id()?.to_owned()); }
        // Cloud may provide an access token without user identity; resolve it via Twitch validate.
        if token.user_id.as_deref().unwrap_or("").is_empty() {
            let res = HttpClient::new().get("https://id.twitch.tv/oauth2/validate")
                .header("Authorization", format!("OAuth {}", token.access_token)).send().await?;
            if !res.status().is_success() { bail!("could not validate cloud-issued Twitch token") }
            let validation: Value = res.json().await?;
            token.user_id = validation["user_id"].as_str().map(str::to_owned);
            token.login = token.login.or_else(|| validation["login"].as_str().map(str::to_owned));
        }
        self.store_token(kind, &token).await?;
        Ok(token)
    }

    pub async fn store_token(&self, kind: AuthKind, token: &StoredToken) -> Result<()> {
        let contents = match tokio::fs::read_to_string(&self.path).await {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "{}".into(),
            Err(e) => return Err(e.into()),
        };
        let mut doc: Value = serde_json::from_str(&contents)?;
        if !doc.is_object() { bail!("integrations.json root is not an object") }
        if doc.get("twitch").is_none_or(Value::is_null) { doc["twitch"] = json!({}); }
        doc["twitch"][kind.as_str()] = serde_json::to_value(token)?;
        if let Some(parent) = self.path.parent() { tokio::fs::create_dir_all(parent).await?; }
        // Persist atomically and avoid leaving partial JSON behind after interruption.
        let tmp = self.path.with_extension(format!("json.{}.tmp", std::process::id()));
        tokio::fs::write(&tmp, serde_json::to_vec_pretty(&doc)?).await?;
        if let Err(e) = tokio::fs::rename(&tmp, &self.path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e.into());
        }
        Ok(())
    }
    // TODO: Cloud-managed token renewal/reauthorization before token expiry.
    // TODO: Serialize concurrent auth callbacks to avoid overwriting token changes.
}

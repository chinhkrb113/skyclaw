//! WhatsApp Cloud API channel — sends and receives messages via Meta's
//! WhatsApp Business Cloud API (Graph API v23.0).
//!
//! Inbound messages arrive via webhook (`POST /webhook/whatsapp`) handled
//! by the gateway.  Outbound messages use `POST /{phone_number_id}/messages`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use serde::Deserialize;
use tokio::sync::mpsc;

use temm1e_core::types::config::ChannelConfig;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::file::{FileData, FileMetadata, OutboundFile, ReceivedFile};
use temm1e_core::types::message::{AttachmentRef, InboundMessage, OutboundMessage};
use temm1e_core::{Channel, FileTransfer};

use crate::whatsapp_common::{
    load_whatsapp_allowlist, normalize_phone, safe_truncate, sanitize_filename,
    save_whatsapp_allowlist, WhatsAppAllowlistFile,
};

// ── Constants ────────────────────────────────────────────────────────

/// WhatsApp text message body limit (4096 characters).
const WA_TEXT_LIMIT: usize = 4096;

/// WhatsApp document upload limit (100 MB).
const WA_DOCUMENT_LIMIT: usize = 100 * 1024 * 1024;

/// Default Graph API version.
const DEFAULT_API_VERSION: &str = "v23.0";

/// Allowlist filename on disk.
const ALLOWLIST_FILE: &str = "whatsapp_allowlist.toml";

// ── Webhook payload types (used by gateway webhook handler) ─────────

#[derive(Debug, Deserialize)]
pub struct WebhookPayload {
    pub entry: Vec<WebhookEntry>,
}

#[derive(Debug, Deserialize)]
pub struct WebhookEntry {
    pub changes: Vec<WebhookChange>,
}

#[derive(Debug, Deserialize)]
pub struct WebhookChange {
    pub field: String,
    pub value: WebhookValue,
}

#[derive(Debug, Deserialize)]
pub struct WebhookValue {
    pub contacts: Option<Vec<WebhookContact>>,
    pub messages: Option<Vec<WebhookMessage>>,
}

#[derive(Debug, Deserialize)]
pub struct WebhookContact {
    pub profile: Option<WebhookProfile>,
    pub wa_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WebhookProfile {
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WebhookMessage {
    pub id: String,
    pub from: String,
    pub timestamp: String,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub text: Option<WebhookText>,
    pub image: Option<WebhookMedia>,
    pub video: Option<WebhookMedia>,
    pub audio: Option<WebhookMedia>,
    pub document: Option<WebhookDocument>,
    pub context: Option<WebhookContext>,
}

#[derive(Debug, Deserialize)]
pub struct WebhookText {
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct WebhookMedia {
    pub id: String,
    pub mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WebhookDocument {
    pub id: String,
    pub mime_type: Option<String>,
    pub filename: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WebhookContext {
    #[serde(rename = "message_id")]
    pub message_id: Option<String>,
}

/// Webhook verification query parameters.
#[derive(Debug, Deserialize)]
pub struct WebhookVerifyParams {
    #[serde(rename = "hub.mode")]
    pub hub_mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    pub hub_verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    pub hub_challenge: Option<String>,
}

// ── Channel struct ───────────────────────────────────────────────────

/// WhatsApp Business Cloud API channel.
pub struct WhatsAppChannel {
    phone_number_id: String,
    access_token: String,
    verify_token: String,
    app_secret: Option<String>,
    api_version: String,
    http_client: reqwest::Client,
    allowlist: Arc<RwLock<Vec<String>>>,
    admin: Arc<RwLock<Option<String>>>,
    tx: mpsc::Sender<InboundMessage>,
    rx: Option<mpsc::Receiver<InboundMessage>>,
    shutdown: Arc<AtomicBool>,
    last_inbound: Arc<RwLock<HashMap<String, chrono::DateTime<chrono::Utc>>>>,
    file_transfer_enabled: bool,
}

impl WhatsAppChannel {
    /// Create a new WhatsApp Cloud API channel.
    ///
    /// Required config fields:
    /// - `token` — format: `{phone_number_id}:{access_token}:{verify_token}` or
    ///   just the access token (with env vars `WHATSAPP_PHONE_NUMBER_ID` and
    ///   `WHATSAPP_VERIFY_TOKEN`).
    pub fn new(config: &ChannelConfig) -> Result<Self, Temm1eError> {
        let token_str = config
            .token
            .clone()
            .ok_or_else(|| Temm1eError::Config("WhatsApp channel requires credentials".into()))?;

        // Parse compound token: "phone_number_id:access_token:verify_token[:app_secret]"
        let parts: Vec<&str> = token_str.splitn(4, ':').collect();
        let (phone_number_id, access_token, verify_token, app_secret) = if parts.len() >= 3 {
            (
                parts[0].to_string(),
                parts[1].to_string(),
                parts[2].to_string(),
                parts.get(3).map(|s| s.to_string()),
            )
        } else {
            // Fallback: token is just the access token; read others from env.
            let phone_id = std::env::var("WHATSAPP_PHONE_NUMBER_ID").map_err(|_| {
                Temm1eError::Config(
                    "WhatsApp: provide compound token (phone_id:token:verify) \
                     or set WHATSAPP_PHONE_NUMBER_ID env var"
                        .into(),
                )
            })?;
            let verify = std::env::var("WHATSAPP_VERIFY_TOKEN").unwrap_or_else(|_| {
                let v = uuid::Uuid::new_v4().to_string();
                tracing::warn!(verify_token = %v, "No WHATSAPP_VERIFY_TOKEN set, generated random");
                v
            });
            let secret = std::env::var("WHATSAPP_APP_SECRET").ok();
            (phone_id, token_str, verify, secret)
        };

        let (tx, rx) = mpsc::channel(256);

        // Load persisted allowlist
        let (allowlist, admin) = if let Some(file) = load_whatsapp_allowlist(ALLOWLIST_FILE) {
            tracing::info!(
                admin = %file.admin,
                users = ?file.users,
                "Loaded WhatsApp allowlist"
            );
            (file.users.clone(), Some(file.admin.clone()))
        } else if !config.allowlist.is_empty() {
            let admin = normalize_phone(&config.allowlist[0]);
            let users: Vec<String> = config
                .allowlist
                .iter()
                .map(|p| normalize_phone(p))
                .collect();
            (users, Some(admin))
        } else {
            (Vec::new(), None)
        };

        Ok(Self {
            phone_number_id,
            access_token,
            verify_token,
            app_secret,
            api_version: DEFAULT_API_VERSION.to_string(),
            http_client: reqwest::Client::new(),
            allowlist: Arc::new(RwLock::new(allowlist)),
            admin: Arc::new(RwLock::new(admin)),
            tx,
            rx: Some(rx),
            shutdown: Arc::new(AtomicBool::new(false)),
            last_inbound: Arc::new(RwLock::new(HashMap::new())),
            file_transfer_enabled: config.file_transfer,
        })
    }

    /// Take the inbound message receiver. The gateway should call this once.
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<InboundMessage>> {
        self.rx.take()
    }

    /// Get the verify token for webhook handshake.
    pub fn verify_token(&self) -> &str {
        &self.verify_token
    }

    /// Get the app secret for webhook signature validation.
    pub fn app_secret(&self) -> Option<&str> {
        self.app_secret.as_deref()
    }

    /// Get a clone of the inbound sender for the webhook handler.
    pub fn tx(&self) -> mpsc::Sender<InboundMessage> {
        self.tx.clone()
    }

    /// Get a clone of the last-inbound tracker for the webhook handler.
    pub fn last_inbound_tracker(
        &self,
    ) -> Arc<RwLock<HashMap<String, chrono::DateTime<chrono::Utc>>>> {
        self.last_inbound.clone()
    }

    /// Get allowlist Arc for the webhook handler.
    pub fn allowlist_arc(&self) -> Arc<RwLock<Vec<String>>> {
        self.allowlist.clone()
    }

    /// Get admin Arc for the webhook handler.
    pub fn admin_arc(&self) -> Arc<RwLock<Option<String>>> {
        self.admin.clone()
    }

    /// Build the Graph API URL for a given endpoint.
    fn api_url(&self, endpoint: &str) -> String {
        format!(
            "https://graph.facebook.com/{}/{}",
            self.api_version, endpoint
        )
    }

    /// Extract text from a webhook message.
    pub fn extract_text(msg: &WebhookMessage) -> Option<String> {
        match msg.msg_type.as_str() {
            "text" => msg.text.as_ref().map(|t| t.body.clone()),
            "image" => Some("[Image received]".to_string()),
            "video" => Some("[Video received]".to_string()),
            "audio" => Some("[Audio received]".to_string()),
            "document" => {
                let name = msg
                    .document
                    .as_ref()
                    .and_then(|d| d.filename.clone())
                    .unwrap_or_else(|| "document".to_string());
                Some(format!("[Document received: {name}]"))
            }
            other => Some(format!("[{other} message received]")),
        }
    }

    /// Extract attachment refs from a webhook message.
    pub fn extract_attachments(msg: &WebhookMessage) -> Vec<AttachmentRef> {
        let mut attachments = Vec::new();
        if let Some(ref img) = msg.image {
            attachments.push(AttachmentRef {
                file_id: img.id.clone(),
                file_name: None,
                mime_type: img.mime_type.clone(),
                size: None,
            });
        }
        if let Some(ref vid) = msg.video {
            attachments.push(AttachmentRef {
                file_id: vid.id.clone(),
                file_name: None,
                mime_type: vid.mime_type.clone(),
                size: None,
            });
        }
        if let Some(ref aud) = msg.audio {
            attachments.push(AttachmentRef {
                file_id: aud.id.clone(),
                file_name: None,
                mime_type: aud.mime_type.clone(),
                size: None,
            });
        }
        if let Some(ref doc) = msg.document {
            attachments.push(AttachmentRef {
                file_id: doc.id.clone(),
                file_name: doc.filename.clone(),
                mime_type: doc.mime_type.clone(),
                size: None,
            });
        }
        attachments
    }

    /// Process an auto-whitelist for the first user.
    pub fn auto_whitelist_if_empty(
        allowlist: &Arc<RwLock<Vec<String>>>,
        admin: &Arc<RwLock<Option<String>>>,
        user_id: &str,
    ) {
        let mut list = allowlist.write().unwrap_or_else(|p| p.into_inner());
        if list.is_empty() {
            list.push(user_id.to_string());
            let mut adm = admin.write().unwrap_or_else(|p| p.into_inner());
            *adm = Some(user_id.to_string());
            tracing::info!(user_id = %user_id, "Auto-whitelisted first WhatsApp user as admin");
            drop(list);
            drop(adm);
            // Persist
            let data = WhatsAppAllowlistFile {
                admin: user_id.to_string(),
                users: vec![user_id.to_string()],
            };
            if let Err(e) = save_whatsapp_allowlist(ALLOWLIST_FILE, &data) {
                tracing::error!(error = %e, "Failed to persist WhatsApp allowlist");
            }
        }
    }
}

// ── Channel trait impl ───────────────────────────────────────────────

#[async_trait]
impl Channel for WhatsAppChannel {
    fn name(&self) -> &str {
        "whatsapp"
    }

    async fn start(&mut self) -> Result<(), Temm1eError> {
        // Cloud API is webhook-based — no persistent connection to maintain.
        // Validate token by making a lightweight API call.
        let url = self.api_url(&self.phone_number_id);
        match self
            .http_client
            .get(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!("WhatsApp Cloud API token validated");
            }
            Ok(resp) => {
                let status = resp.status();
                tracing::warn!(
                    status = %status,
                    "WhatsApp token validation returned non-200 — channel will start anyway"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "WhatsApp token validation failed — channel will start anyway"
                );
            }
        }

        tracing::info!(
            phone_number_id = %self.phone_number_id,
            "WhatsApp Cloud API channel started (webhook-based)"
        );
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Temm1eError> {
        self.shutdown.store(true, Ordering::Relaxed);
        tracing::info!("WhatsApp Cloud API channel stopped");
        Ok(())
    }

    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
        // Check 24h window
        let in_window = {
            let windows = self.last_inbound.read().unwrap_or_else(|p| p.into_inner());
            windows
                .get(&msg.chat_id)
                .map(|t| chrono::Utc::now() - *t < chrono::Duration::hours(24))
                .unwrap_or(false)
        };

        if !in_window {
            tracing::warn!(
                chat_id = %msg.chat_id,
                "Sending outside 24h service window — may require template message"
            );
        }

        // UTF-8 safe truncation
        let text = safe_truncate(&msg.text, WA_TEXT_LIMIT);

        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": msg.chat_id,
            "type": "text",
            "text": { "body": text }
        });

        let url = self.api_url(&format!("{}/messages", self.phone_number_id));

        let resp = self
            .http_client
            .post(&url)
            .bearer_auth(&self.access_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| Temm1eError::Channel(format!("WhatsApp send failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            return Err(Temm1eError::Channel(format!(
                "WhatsApp API {status}: {resp_body}"
            )));
        }

        Ok(())
    }

    fn is_allowed(&self, user_id: &str) -> bool {
        let list = self.allowlist.read().unwrap_or_else(|p| p.into_inner());
        if list.is_empty() {
            return false; // DF-16: empty allowlist = deny all
        }
        if list.iter().any(|a| a == "*") {
            return true;
        }
        let normalized = normalize_phone(user_id);
        list.iter().any(|a| normalize_phone(a) == normalized)
    }

    fn file_transfer(&self) -> Option<&dyn FileTransfer> {
        if self.file_transfer_enabled {
            Some(self)
        } else {
            None
        }
    }

    async fn delete_message(&self, _chat_id: &str, _message_id: &str) -> Result<(), Temm1eError> {
        // WhatsApp Cloud API does not support message deletion by the business.
        Ok(())
    }
}

// ── FileTransfer trait impl ──────────────────────────────────────────

#[async_trait]
impl FileTransfer for WhatsAppChannel {
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>, Temm1eError> {
        let mut files = Vec::new();
        for att in &msg.attachments {
            // Step 1: Get the media URL
            let media_url = self.api_url(&att.file_id);
            let meta_resp = self
                .http_client
                .get(&media_url)
                .bearer_auth(&self.access_token)
                .send()
                .await
                .map_err(|e| Temm1eError::FileTransfer(format!("Failed to get media URL: {e}")))?;

            let meta_json: serde_json::Value = meta_resp.json().await.map_err(|e| {
                Temm1eError::FileTransfer(format!("Failed to parse media metadata: {e}"))
            })?;

            let download_url = meta_json["url"].as_str().ok_or_else(|| {
                Temm1eError::FileTransfer("No download URL in media response".into())
            })?;

            // Step 2: Download the actual file
            let file_resp = self
                .http_client
                .get(download_url)
                .bearer_auth(&self.access_token)
                .send()
                .await
                .map_err(|e| Temm1eError::FileTransfer(format!("Failed to download media: {e}")))?;

            let data = file_resp.bytes().await.map_err(|e| {
                Temm1eError::FileTransfer(format!("Failed to read media bytes: {e}"))
            })?;

            let name = att
                .file_name
                .clone()
                .map(|n| sanitize_filename(&n))
                .unwrap_or_else(|| format!("file_{}", att.file_id));

            files.push(ReceivedFile {
                name,
                mime_type: att
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| "application/octet-stream".to_string()),
                size: data.len(),
                data,
            });
        }
        Ok(files)
    }

    async fn send_file(&self, chat_id: &str, file: OutboundFile) -> Result<(), Temm1eError> {
        // Step 1: Upload media
        let upload_url = self.api_url(&format!("{}/media", self.phone_number_id));

        let file_bytes = match &file.data {
            FileData::Bytes(b) => b.clone(),
            FileData::Url(url) => {
                // Download from URL first
                let resp = self.http_client.get(url).send().await.map_err(|e| {
                    Temm1eError::FileTransfer(format!("Failed to fetch file from URL: {e}"))
                })?;
                resp.bytes().await.map_err(|e| {
                    Temm1eError::FileTransfer(format!("Failed to read file bytes: {e}"))
                })?
            }
        };

        let part = reqwest::multipart::Part::bytes(file_bytes.to_vec())
            .file_name(sanitize_filename(&file.name))
            .mime_str(&file.mime_type)
            .map_err(|e| Temm1eError::FileTransfer(format!("Invalid MIME type: {e}")))?;

        let form = reqwest::multipart::Form::new()
            .text("messaging_product", "whatsapp")
            .text("type", file.mime_type.clone())
            .part("file", part);

        let upload_resp = self
            .http_client
            .post(&upload_url)
            .bearer_auth(&self.access_token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| Temm1eError::FileTransfer(format!("Media upload failed: {e}")))?;

        let upload_json: serde_json::Value = upload_resp.json().await.map_err(|e| {
            Temm1eError::FileTransfer(format!("Failed to parse upload response: {e}"))
        })?;

        let media_id = upload_json["id"]
            .as_str()
            .ok_or_else(|| Temm1eError::FileTransfer("No media ID in upload response".into()))?;

        // Step 2: Send the media message
        let msg_type = if file.mime_type.starts_with("image/") {
            "image"
        } else if file.mime_type.starts_with("video/") {
            "video"
        } else if file.mime_type.starts_with("audio/") {
            "audio"
        } else {
            "document"
        };

        let mut media_obj = serde_json::json!({ "id": media_id });
        if let Some(ref caption) = file.caption {
            media_obj["caption"] = serde_json::json!(caption);
        }
        if msg_type == "document" {
            media_obj["filename"] = serde_json::json!(sanitize_filename(&file.name));
        }

        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": chat_id,
            "type": msg_type,
            msg_type: media_obj,
        });

        let url = self.api_url(&format!("{}/messages", self.phone_number_id));
        let resp = self
            .http_client
            .post(&url)
            .bearer_auth(&self.access_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| Temm1eError::FileTransfer(format!("Failed to send media: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Temm1eError::FileTransfer(format!(
                "WhatsApp media send {status}: {body}"
            )));
        }

        Ok(())
    }

    async fn send_file_stream(
        &self,
        chat_id: &str,
        stream: BoxStream<'_, Bytes>,
        metadata: FileMetadata,
    ) -> Result<(), Temm1eError> {
        // WhatsApp doesn't support streaming uploads — collect and send.
        use futures::StreamExt;
        let mut buf = Vec::new();
        let mut stream = stream;
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk);
        }

        self.send_file(
            chat_id,
            OutboundFile {
                name: metadata.name,
                mime_type: metadata.mime_type,
                data: FileData::Bytes(Bytes::from(buf)),
                caption: None,
            },
        )
        .await
    }

    fn max_file_size(&self) -> usize {
        WA_DOCUMENT_LIMIT
    }
}

// ── Webhook signature validation ─────────────────────────────────────

/// Validate the X-Hub-Signature-256 header from Meta's webhook.
pub fn validate_webhook_signature(body: &[u8], signature_header: &str, app_secret: &str) -> bool {
    let expected_hex = match signature_header.strip_prefix("sha256=") {
        Some(hex) => hex,
        None => return false,
    };

    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut mac = match Hmac::<Sha256>::new_from_slice(app_secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let computed = hex::encode(mac.finalize().into_bytes());

    // Constant-time-ish comparison
    computed.len() == expected_hex.len()
        && computed
            .bytes()
            .zip(expected_hex.bytes())
            .all(|(a, b)| a == b)
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(token: Option<&str>) -> ChannelConfig {
        ChannelConfig {
            enabled: true,
            token: token.map(|t| t.to_string()),
            allowlist: Vec::new(),
            file_transfer: true,
            max_file_size: None,
        }
    }

    #[test]
    fn create_requires_token() {
        let config = test_config(None);
        let result = WhatsAppChannel::new(&config);
        assert!(result.is_err());
    }

    #[test]
    fn create_with_compound_token() {
        let config = test_config(Some("12345:access_tok:verify_tok"));
        let channel = WhatsAppChannel::new(&config).unwrap();
        assert_eq!(channel.phone_number_id, "12345");
        assert_eq!(channel.access_token, "access_tok");
        assert_eq!(channel.verify_token, "verify_tok");
    }

    #[test]
    fn create_with_compound_token_and_secret() {
        let config = test_config(Some("12345:access_tok:verify_tok:my_secret"));
        let channel = WhatsAppChannel::new(&config).unwrap();
        assert_eq!(channel.app_secret.as_deref(), Some("my_secret"));
    }

    #[test]
    fn channel_name() {
        let config = test_config(Some("12345:tok:verify"));
        let channel = WhatsAppChannel::new(&config).unwrap();
        assert_eq!(channel.name(), "whatsapp");
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let config = test_config(Some("12345:tok:verify"));
        let channel = WhatsAppChannel::new(&config).unwrap();
        assert!(!channel.is_allowed("15551234567"));
    }

    #[test]
    fn allowlist_matches_phone_numbers() {
        let mut config = test_config(Some("12345:tok:verify"));
        config.allowlist = vec!["+1 555 123 4567".to_string()];
        let channel = WhatsAppChannel::new(&config).unwrap();
        assert!(channel.is_allowed("+15551234567"));
        assert!(channel.is_allowed("15551234567"));
        assert!(!channel.is_allowed("9999999999"));
    }

    #[test]
    fn wildcard_allowlist() {
        let mut config = test_config(Some("12345:tok:verify"));
        config.allowlist = vec!["*".to_string()];
        let channel = WhatsAppChannel::new(&config).unwrap();
        assert!(channel.is_allowed("anyone"));
    }

    #[test]
    fn take_receiver_once() {
        let config = test_config(Some("12345:tok:verify"));
        let mut channel = WhatsAppChannel::new(&config).unwrap();
        assert!(channel.take_receiver().is_some());
        assert!(channel.take_receiver().is_none());
    }

    #[test]
    fn file_transfer_available() {
        let config = test_config(Some("12345:tok:verify"));
        let channel = WhatsAppChannel::new(&config).unwrap();
        assert!(channel.file_transfer().is_some());
    }

    #[test]
    fn file_transfer_disabled() {
        let mut config = test_config(Some("12345:tok:verify"));
        config.file_transfer = false;
        let channel = WhatsAppChannel::new(&config).unwrap();
        assert!(channel.file_transfer().is_none());
    }

    #[test]
    fn extract_text_message() {
        let msg = WebhookMessage {
            id: "msg1".into(),
            from: "15551234567".into(),
            timestamp: "1234567890".into(),
            msg_type: "text".into(),
            text: Some(WebhookText {
                body: "Hello!".into(),
            }),
            image: None,
            video: None,
            audio: None,
            document: None,
            context: None,
        };
        assert_eq!(
            WhatsAppChannel::extract_text(&msg),
            Some("Hello!".to_string())
        );
    }

    #[test]
    fn extract_document_text() {
        let msg = WebhookMessage {
            id: "msg2".into(),
            from: "15551234567".into(),
            timestamp: "1234567890".into(),
            msg_type: "document".into(),
            text: None,
            image: None,
            video: None,
            audio: None,
            document: Some(WebhookDocument {
                id: "media1".into(),
                mime_type: Some("application/pdf".into()),
                filename: Some("report.pdf".into()),
            }),
            context: None,
        };
        assert_eq!(
            WhatsAppChannel::extract_text(&msg),
            Some("[Document received: report.pdf]".to_string())
        );
    }

    #[test]
    fn extract_attachments_image() {
        let msg = WebhookMessage {
            id: "msg3".into(),
            from: "15551234567".into(),
            timestamp: "1234567890".into(),
            msg_type: "image".into(),
            text: None,
            image: Some(WebhookMedia {
                id: "img_123".into(),
                mime_type: Some("image/jpeg".into()),
            }),
            video: None,
            audio: None,
            document: None,
            context: None,
        };
        let atts = WhatsAppChannel::extract_attachments(&msg);
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].file_id, "img_123");
        assert_eq!(atts[0].mime_type.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn webhook_signature_valid() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let secret = "test_secret";
        let body = b"test body";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        assert!(validate_webhook_signature(body, &sig, secret));
    }

    #[test]
    fn webhook_signature_invalid() {
        assert!(!validate_webhook_signature(
            b"body",
            "sha256=deadbeef",
            "secret"
        ));
    }

    #[test]
    fn webhook_signature_missing_prefix() {
        assert!(!validate_webhook_signature(b"body", "deadbeef", "secret"));
    }

    #[test]
    fn webhook_verify_params_deserialize() {
        let json = r#"{"hub.mode":"subscribe","hub.verify_token":"my_token","hub.challenge":"challenge_123"}"#;
        let params: WebhookVerifyParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.hub_mode.as_deref(), Some("subscribe"));
        assert_eq!(params.hub_verify_token.as_deref(), Some("my_token"));
        assert_eq!(params.hub_challenge.as_deref(), Some("challenge_123"));
    }
}

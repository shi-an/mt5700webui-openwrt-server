use crate::config::NotificationConfig;
use anyhow::Result;
use async_trait::async_trait;
use log::{error, info, warn};
use reqwest::Client;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use urlencoding::encode;

#[derive(Debug, Clone)]
pub struct NotificationMessage {
    pub sender: String,
    pub content: String,
    pub notification_type: NotificationType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationType {
    SMS,
    Call,
    MemoryFull,
    Signal,
}

#[async_trait]
pub trait NotificationChannel: Send + Sync {
    async fn send(&self, msg: &NotificationMessage) -> Result<()>;
}

struct LogNotification {
    tx: mpsc::Sender<String>,
}

impl LogNotification {
    fn new(persist: bool) -> Self {
        let log_path = if persist {
            PathBuf::from("/var/log/at-notifications.log")
        } else {
            PathBuf::from("/tmp/at-notifications.log")
        };

        if let Some(parent) = log_path.parent() {
            if !parent.exists() {
                let _ = std::fs::create_dir_all(parent);
            }
        }

        // Create empty file if not exists
        let _ = std::fs::OpenOptions::new().create(true).write(true).open(&log_path);
        info!("Notification logging enabled at: {:?}", log_path);

        let (tx, mut rx) = mpsc::channel::<String>(100);
        let path_clone = log_path.clone();

        tokio::spawn(async move {
            let mut buffer = String::with_capacity(10240);
            let mut interval = interval(Duration::from_secs(5));

            loop {
                tokio::select! {
                    Some(msg) = rx.recv() => {
                        buffer.push_str(&msg);
                        if buffer.len() > 8192 {
                            Self::flush(&path_clone, &mut buffer).await;
                        }
                    }
                    _ = interval.tick() => {
                        if !buffer.is_empty() {
                            Self::flush(&path_clone, &mut buffer).await;
                        }
                    }
                }
            }
        });

        Self { tx }
    }

    async fn flush(path: &PathBuf, buffer: &mut String) {
        // Rotate if needed (1MB limit)
        if let Ok(metadata) = fs::metadata(path).await {
            if metadata.len() > 1024 * 1024 {
                let mut bak_path = path.clone();
                bak_path.set_extension("log.bak");
                let _ = fs::rename(path, bak_path).await;
            }
        }

        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path).await {
            if let Err(e) = file.write_all(buffer.as_bytes()).await {
                error!("Failed to write notification log: {}", e);
            }
        }
        buffer.clear();
    }
}

#[async_trait]
impl NotificationChannel for LogNotification {
    async fn send(&self, msg: &NotificationMessage) -> Result<()> {
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let line = format!("[{}] [{:?}] {}: {}\n", timestamp, msg.notification_type, msg.sender, msg.content);
        let _ = self.tx.send(line).await;
        Ok(())
    }
}

// 1. WeChat (Original implementation, kept for compatibility if needed, but we will use the new one)
// Actually, we can reuse the batch logic for all providers if we want, but requirements asked for specific implementations.
// The requirement asks to implement 9 channels + custom script.
// We will implement them simply without batching for now as per requirement, or keep batching for WeChat if desired.
// For simplicity and to match the requirements "use reqwest::Client, log::warn!, non-blocking", we will implement direct sending.

struct PushPlus { token: String, client: Client }
#[async_trait]
impl NotificationChannel for PushPlus {
    async fn send(&self, msg: &NotificationMessage) -> Result<()> {
        let url = "http://www.pushplus.plus/send";
        let payload = serde_json::json!({
            "token": self.token,
            "title": msg.sender,
            "content": msg.content
        });
        let client = self.client.clone();
        tokio::spawn(async move {
            if let Err(e) = client.post(url).json(&payload).send().await {
                warn!("PushPlus notification failed: {}", e);
            }
        });
        Ok(())
    }
}

struct ServerChan { key: String, client: Client }
#[async_trait]
impl NotificationChannel for ServerChan {
    async fn send(&self, msg: &NotificationMessage) -> Result<()> {
        let url = format!("https://sctapi.ftqq.com/{}.send", self.key);
        let sender = msg.sender.clone();
        let content = msg.content.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            let params = [("title", sender), ("desp", content)];
            if let Err(e) = client.post(url).form(&params).send().await {
                warn!("ServerChan notification failed: {}", e);
            }
        });
        Ok(())
    }
}

struct PushDeer { key: String, url: String, client: Client }
#[async_trait]
impl NotificationChannel for PushDeer {
    async fn send(&self, msg: &NotificationMessage) -> Result<()> {
        let base_url = if self.url.is_empty() { "https://api2.pushdeer.com" } else { &self.url };
        let url = format!("{}/message/push", base_url.trim_end_matches('/'));
        let payload = serde_json::json!({
            "pushkey": self.key,
            "text": msg.sender,
            "desp": msg.content
        });
        let client = self.client.clone();
        tokio::spawn(async move {
            if let Err(e) = client.post(url).json(&payload).send().await {
                warn!("PushDeer notification failed: {}", e);
            }
        });
        Ok(())
    }
}

struct Feishu { webhook: String, client: Client }
#[async_trait]
impl NotificationChannel for Feishu {
    async fn send(&self, msg: &NotificationMessage) -> Result<()> {
        let payload = serde_json::json!({
            "msg_type": "text",
            "content": {
                "text": format!("{}\n{}", msg.sender, msg.content)
            }
        });
        let client = self.client.clone();
        let url = self.webhook.clone();
        tokio::spawn(async move {
            if let Err(e) = client.post(url).json(&payload).send().await {
                warn!("Feishu notification failed: {}", e);
            }
        });
        Ok(())
    }
}

struct DingTalk { webhook: String, _secret: Option<String>, client: Client }
#[async_trait]
impl NotificationChannel for DingTalk {
    async fn send(&self, msg: &NotificationMessage) -> Result<()> {
        let url = self.webhook.clone();
        if let Some(_secret) = &self._secret {
            // DingTalk signature logic could be added here if needed, but requirements just mentioned webhook/secret config
            // Simple appending if user provided a signed URL or just ignore secret if not implementing signing logic
            // For now, we assume webhook contains token. If signing is needed, it requires timestamp + secret HMAC
            // Requirement said "DingTalk: POST webhook_url, JSON ...".
            // We will skip complex signing for now unless strictly required, as it requires chrono timestamp and hmac-sha256
        }

        let payload = serde_json::json!({
            "msgtype": "text",
            "text": {
                "content": format!("{}\n{}", msg.sender, msg.content)
            }
        });
        let client = self.client.clone();
        tokio::spawn(async move {
            if let Err(e) = client.post(url).json(&payload).send().await {
                warn!("DingTalk notification failed: {}", e);
            }
        });
        Ok(())
    }
}

struct Bark { url: String, client: Client }
#[async_trait]
impl NotificationChannel for Bark {
    async fn send(&self, msg: &NotificationMessage) -> Result<()> {
        let base_url = self.url.trim_end_matches('/');
        let sender = encode(&msg.sender);
        let content = encode(&msg.content);
        let url = format!("{}/{}/{}", base_url, sender, content);
        let client = self.client.clone();
        tokio::spawn(async move {
            if let Err(e) = client.get(url).send().await {
                warn!("Bark notification failed: {}", e);
            }
        });
        Ok(())
    }
}

struct Telegram { token: String, chat_id: String, client: Client }
#[async_trait]
impl NotificationChannel for Telegram {
    async fn send(&self, msg: &NotificationMessage) -> Result<()> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.token);
        let payload = serde_json::json!({
            "chat_id": self.chat_id,
            "text": format!("{}\n{}", msg.sender, msg.content)
        });
        let client = self.client.clone();
        tokio::spawn(async move {
            if let Err(e) = client.post(url).json(&payload).send().await {
                warn!("Telegram notification failed: {}", e);
            }
        });
        Ok(())
    }
}

struct GenericWebhook { url: String, client: Client }
#[async_trait]
impl NotificationChannel for GenericWebhook {
    async fn send(&self, msg: &NotificationMessage) -> Result<()> {
        let payload = serde_json::json!({
            "title": msg.sender,
            "content": msg.content
        });
        let client = self.client.clone();
        let url = self.url.clone();
        tokio::spawn(async move {
            if let Err(e) = client.post(url).json(&payload).send().await {
                warn!("Generic Webhook notification failed: {}", e);
            }
        });
        Ok(())
    }
}

struct CustomScript { path: String }
#[async_trait]
impl NotificationChannel for CustomScript {
    async fn send(&self, msg: &NotificationMessage) -> Result<()> {
        let path = self.path.clone();
        let sender = msg.sender.clone();
        let content = msg.content.clone();
        tokio::spawn(async move {
            if let Err(e) = Command::new(path)
                .arg(sender)
                .arg(content)
                .status()
                .await 
            {
                warn!("Custom script execution failed: {}", e);
            }
        });
        Ok(())
    }
}

// Re-implement WeChat to match new style (without batching for consistency, or keep batching if preferred. 
// Requirement said "ensure... enabled... instantiate...".
// We will use the GenericWebhook style for WeChat too if "wechat" is enabled via enabled_push_services.
// But for backward compatibility with existing code structure:
struct WeChatWork { webhook: String, client: Client }
#[async_trait]
impl NotificationChannel for WeChatWork {
    async fn send(&self, msg: &NotificationMessage) -> Result<()> {
        let payload = serde_json::json!({
            "msgtype": "text",
            "text": {
                "content": format!("{}\n{}", msg.sender, msg.content)
            }
        });
        let client = self.client.clone();
        let url = self.webhook.clone();
        tokio::spawn(async move {
            if let Err(e) = client.post(url).json(&payload).send().await {
                warn!("WeChat notification failed: {}", e);
            }
        });
        Ok(())
    }
}

#[derive(Clone)]
pub struct NotificationManager {
    channels: Arc<Vec<Box<dyn NotificationChannel>>>,
    config: Arc<NotificationConfig>,
}

impl NotificationManager {
    pub fn new(config: NotificationConfig) -> Self {
        let mut channels: Vec<Box<dyn NotificationChannel>> = Vec::new();
        let client = Client::new();
        
        // Initialize Log Notification
        if config.notify_log_enable {
             let logger = LogNotification::new(config.notify_log_persist);
             channels.push(Box::new(logger));
        }
        
        // Check enabled services
        for service in &config.enabled_push_services {
            match service.as_str() {
                "wechat" => {
                    if let Some(url) = &config.wechat_webhook {
                        channels.push(Box::new(WeChatWork { webhook: url.clone(), client: client.clone() }));
                        info!("已启用 企业微信 推送");
                    }
                },
                "pushplus" => {
                    if let Some(token) = &config.pushplus_token {
                        channels.push(Box::new(PushPlus { token: token.clone(), client: client.clone() }));
                        info!("已启用 PushPlus 推送");
                    }
                },
                "serverchan" => {
                    if let Some(key) = &config.serverchan_key {
                        channels.push(Box::new(ServerChan { key: key.clone(), client: client.clone() }));
                        info!("已启用 Server酱 推送");
                    }
                },
                "pushdeer" => {
                    if let Some(key) = &config.pushdeer_key {
                        let url = config.pushdeer_url.clone().unwrap_or_default();
                        channels.push(Box::new(PushDeer { key: key.clone(), url, client: client.clone() }));
                        info!("已启用 PushDeer 推送");
                    }
                },
                "feishu" => {
                    if let Some(url) = &config.feishu_webhook {
                        channels.push(Box::new(Feishu { webhook: url.clone(), client: client.clone() }));
                        info!("已启用 飞书 推送");
                    }
                },
                "dingtalk" => {
                    if let Some(url) = &config.dingtalk_webhook {
                        channels.push(Box::new(DingTalk { webhook: url.clone(), _secret: config.dingtalk_secret.clone(), client: client.clone() }));
                        info!("已启用 钉钉 推送");
                    }
                },
                "bark" => {
                    if let Some(url) = &config.bark_url {
                        channels.push(Box::new(Bark { url: url.clone(), client: client.clone() }));
                        info!("已启用 Bark 推送");
                    }
                },
                "telegram" => {
                    if let (Some(token), Some(chat_id)) = (&config.tg_bot_token, &config.tg_chat_id) {
                        channels.push(Box::new(Telegram { token: token.clone(), chat_id: chat_id.clone(), client: client.clone() }));
                        info!("已启用 Telegram 推送");
                    }
                },
                "generic" => {
                    if let Some(url) = &config.generic_webhook_url {
                        channels.push(Box::new(GenericWebhook { url: url.clone(), client: client.clone() }));
                        info!("已启用 通用Webhook 推送");
                    }
                },
                "custom" => {
                    if let Some(path) = &config.custom_script_path {
                        channels.push(Box::new(CustomScript { path: path.clone() }));
                        info!("已启用 自定义脚本 推送");
                    }
                },
                _ => {}
            }
        }
        
        // Legacy fallback: if wechat_webhook is present but 'wechat' not in enabled list, enable it anyway for backward compatibility?
        // Or strictly follow enabled_push_services. The requirement implies strict checking.
        // But the previous implementation had wechat enabled if config.wechat_webhook was Some.
        // We'll stick to enabled_push_services check as per requirement 4.
        
        Self {
            channels: Arc::new(channels),
            config: Arc::new(config),
        }
    }

    pub async fn notify(&self, sender: &str, content: &str, notification_type: NotificationType) {
        let should_notify = match notification_type {
            NotificationType::SMS => self.config.notify_sms,
            NotificationType::Call => self.config.notify_call,
            NotificationType::MemoryFull => self.config.notify_memory_full_threshold > 0,
            NotificationType::Signal => {
                // signal_threshold == 0 表示禁用信号通知
                // signal_threshold > 0 表示 RSRP 低于 -threshold dBm 才通知
                // 例如 threshold=100 表示 RSRP < -100 dBm 时通知
                let threshold = self.config.notify_signal_threshold;
                if threshold <= 0 {
                    false
                } else {
                    // 从 content 中提取 RSRP 值判断是否超过阈值
                    // content 格式包含 "RSRP: -95 dBm" 等字符串
                    let rsrp = content
                        .lines()
                        .find(|l| l.contains("RSRP:"))
                        .and_then(|l| {
                            l.split_whitespace()
                                .skip_while(|s| *s != "RSRP:")
                                .nth(1)
                                .and_then(|s| s.parse::<i32>().ok())
                        });
                    // RSRP 是负数，低于 -threshold 才通知
                    // 例如 threshold=100：rsrp < -100 时通知
                    rsrp.map(|r| r < -threshold).unwrap_or(true)
                }
            }
        };

        if should_notify {
            let msg = NotificationMessage {
                sender: sender.to_string(),
                content: content.to_string(),
                notification_type,
            };
            
            for channel in self.channels.iter() {
                if let Err(e) = channel.send(&msg).await {
                    error!("Failed to send notification: {}", e);
                }
            }
        }
    }

    pub fn has_active_push_services(&self) -> bool {
        // 判断是否有除了 LogNotification 之外的活跃推送通道
        // 因为 LogNotification 只是写本地文件，不算“转发”
        // 这里我们简单判断 enabled_push_services 是否为空
        !self.config.enabled_push_services.is_empty()
    }
}

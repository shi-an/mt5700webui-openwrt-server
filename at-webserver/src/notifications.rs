use crate::config::NotificationConfig;
use anyhow::Result;
use log::{error, info};
use reqwest::Client;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct NotificationManager {
    config: Arc<NotificationConfig>,
    sender: mpsc::Sender<NotificationMessage>,
}

#[derive(Debug)]
struct NotificationMessage {
    sender: String,
    content: String,
    notification_type: NotificationType,
}

#[derive(Debug)]
pub enum NotificationType {
    SMS,
    Call,
    MemoryFull,
    Signal,
}

impl NotificationManager {
    pub fn new(config: NotificationConfig) -> Self {
        let (tx, mut rx) = mpsc::channel(100);
        let config = Arc::new(config);
        let config_clone = config.clone();

        tokio::spawn(async move {
            let client = Client::new();
            while let Some(msg) = rx.recv().await {
                Self::process_message(&client, &config_clone, msg).await;
            }
        });

        Self {
            config,
            sender: tx,
        }
    }

    pub async fn notify(&self, sender: &str, content: &str, notification_type: NotificationType) {
        let should_notify = match notification_type {
            NotificationType::SMS => self.config.notify_sms,
            NotificationType::Call => self.config.notify_call,
            NotificationType::MemoryFull => self.config.notify_memory_full,
            NotificationType::Signal => self.config.notify_signal,
        };

        if should_notify {
            let msg = NotificationMessage {
                sender: sender.to_string(),
                content: content.to_string(),
                notification_type,
            };
            let _ = self.sender.send(msg).await;
        }
    }

    async fn process_message(client: &Client, config: &NotificationConfig, msg: NotificationMessage) {
        info!("Notification: [{:?}] {}: {}", msg.notification_type, msg.sender, msg.content);
        
        // File Logging
        if let Some(log_file) = &config.log_file {
            if let Err(e) = Self::log_to_file(log_file, &msg) {
                error!("Failed to log notification: {}", e);
            }
        }

        // WeChat Notification
        if let Some(webhook) = &config.wechat_webhook {
            if let Err(e) = Self::send_wechat(client, webhook, &msg).await {
                error!("Failed to send WeChat notification: {}", e);
            }
        }
    }

    fn log_to_file(path: &str, msg: &NotificationMessage) -> Result<()> {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        writeln!(file, "[{}] [{:?}] {}: {}", timestamp, msg.notification_type, msg.sender, msg.content)?;
        Ok(())
    }

    async fn send_wechat(client: &Client, webhook: &str, msg: &NotificationMessage) -> Result<()> {
        let content = format!("# Notification\nType: {:?}\nSender: {}\nContent: {}", 
            msg.notification_type, msg.sender, msg.content);
        
        let payload = serde_json::json!({
            "msgtype": "markdown",
            "markdown": {
                "content": content
            }
        });

        client.post(webhook).json(&payload).send().await?.error_for_status()?;
        Ok(())
    }
}

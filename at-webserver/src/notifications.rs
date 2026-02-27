use crate::config::NotificationConfig;
use anyhow::Result;
use async_trait::async_trait;
use log::{error, info};
use reqwest::Client;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

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
    log_file: PathBuf,
}

impl LogNotification {
    fn new(log_file: String) -> Result<Self> {
        let path = PathBuf::from(log_file);
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
                info!("Created log directory: {:?}", parent);
            }
        }
        Ok(Self { log_file: path })
    }
}

#[async_trait]
impl NotificationChannel for LogNotification {
    async fn send(&self, msg: &NotificationMessage) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)
            .await?;
            
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let line = format!("[{}] [{:?}] {}: {}\n", timestamp, msg.notification_type, msg.sender, msg.content);
        file.write_all(line.as_bytes()).await?;
        Ok(())
    }
}

struct WeChatNotification {
    sender: mpsc::Sender<NotificationMessage>,
}

impl WeChatNotification {
    fn new(webhook_url: String) -> Self {
        let (tx, mut rx) = mpsc::channel::<NotificationMessage>(100);
        let client = Client::new();
        let webhook_url = webhook_url.clone();
        
        tokio::spawn(async move {
            let mut buffer = Vec::new();
            // Send batch every 60 seconds
            let mut ticker = interval(Duration::from_secs(60));
            
            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Some(m) => {
                                buffer.push(m);
                                if buffer.len() >= 10 { // Max batch size
                                    Self::flush(&client, &webhook_url, &mut buffer).await;
                                }
                            }
                            None => {
                                // Channel closed, flush remaining and exit
                                Self::flush(&client, &webhook_url, &mut buffer).await;
                                break;
                            }
                        }
                    }
                    _ = ticker.tick() => {
                        if !buffer.is_empty() {
                            Self::flush(&client, &webhook_url, &mut buffer).await;
                        }
                    }
                }
            }
        });
        
        Self { sender: tx }
    }
    
    async fn flush(client: &Client, webhook: &str, buffer: &mut Vec<NotificationMessage>) {
        if buffer.is_empty() { return; }
        
        let content = buffer.iter().map(|msg| {
             format!("Type: {:?}\nSender: {}\nContent: {}", msg.notification_type, msg.sender, msg.content)
        }).collect::<Vec<_>>().join("\n---\n");
        
        let full_content = format!("# Notification Batch\n{}", content);

        let payload = serde_json::json!({
            "msgtype": "markdown",
            "markdown": {
                "content": full_content
            }
        });
        
        match client.post(webhook).json(&payload).send().await {
            Ok(resp) => {
                if let Err(e) = resp.error_for_status() {
                    error!("WeChat notification failed: {}", e);
                } else {
                    info!("Sent {} WeChat notifications", buffer.len());
                }
            }
            Err(e) => error!("WeChat request failed: {}", e),
        }
        
        buffer.clear();
    }
}

#[async_trait]
impl NotificationChannel for WeChatNotification {
    async fn send(&self, msg: &NotificationMessage) -> Result<()> {
        self.sender.send(msg.clone()).await.map_err(|_| anyhow::anyhow!("Channel closed"))?;
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
        
        // Initialize Log Notification
        if let Some(log_file) = &config.log_file {
             match LogNotification::new(log_file.clone()) {
                 Ok(logger) => {
                     channels.push(Box::new(logger));
                     info!("Log notification enabled: {}", log_file);
                 },
                 Err(e) => error!("Failed to initialize log notification: {}", e),
             }
        }
        
        // Initialize WeChat Notification
        if let Some(webhook) = &config.wechat_webhook {
            channels.push(Box::new(WeChatNotification::new(webhook.clone())));
            info!("WeChat notification enabled");
        }
        
        Self {
            channels: Arc::new(channels),
            config: Arc::new(config),
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
            
            for channel in self.channels.iter() {
                if let Err(e) = channel.send(&msg).await {
                    error!("Failed to send notification: {}", e);
                }
            }
        }
    }
}

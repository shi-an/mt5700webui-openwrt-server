use crate::models::CommandSender;
use crate::notifications::{NotificationManager, NotificationType};
use async_trait::async_trait;
use log::{info, error};
use tokio::sync::oneshot;
use regex::Regex;

#[async_trait]
pub trait MessageHandler: Send + Sync {
    fn can_handle(&self, line: &str) -> bool;
    async fn handle(&self, line: &str, notifications: &NotificationManager, cmd_tx: &CommandSender) -> anyhow::Result<()>;
}

pub struct CallHandler;
#[async_trait]
impl MessageHandler for CallHandler {
    fn can_handle(&self, line: &str) -> bool {
        line.contains("RING") || line.contains("+CLIP:")
    }
    async fn handle(&self, line: &str, notifications: &NotificationManager, _cmd_tx: &CommandSender) -> anyhow::Result<()> {
        if line.contains("RING") {
             notifications.notify("System", "Incoming Call (Ring)", NotificationType::Call).await;
        } else if line.contains("+CLIP:") {
             let re = Regex::new(r#"\+CLIP: "([^"]+)""#).unwrap();
             if let Some(caps) = re.captures(line) {
                 if let Some(number) = caps.get(1) {
                     notifications.notify(number.as_str(), "Incoming Call", NotificationType::Call).await;
                 }
             }
        }
        Ok(())
    }
}

pub struct MemoryFullHandler;
#[async_trait]
impl MessageHandler for MemoryFullHandler {
    fn can_handle(&self, line: &str) -> bool {
        line.contains("+CIEV: \"MESSAGE\",0") || line.contains("+CMS ERROR: 322")
    }
    async fn handle(&self, _line: &str, notifications: &NotificationManager, _cmd_tx: &CommandSender) -> anyhow::Result<()> {
        notifications.notify("System", "SMS Memory Full", NotificationType::MemoryFull).await;
        Ok(())
    }
}

pub struct NewSMSHandler;
#[async_trait]
impl MessageHandler for NewSMSHandler {
    fn can_handle(&self, line: &str) -> bool {
        line.contains("+CMTI:")
    }
    async fn handle(&self, line: &str, notifications: &NotificationManager, cmd_tx: &CommandSender) -> anyhow::Result<()> {
        // +CMTI: "SM", 5
        let re = Regex::new(r#"\+CMTI: "(\w+)",\s*(\d+)"#).unwrap();
        if let Some(caps) = re.captures(line) {
            let index = caps.get(2).map_or("0", |m| m.as_str());
            info!("New SMS at index {}", index);
            
            let cmd = format!("AT+CMGR={}", index);
            let (tx, rx) = oneshot::channel();
            if let Err(_) = cmd_tx.send((cmd, tx)).await {
                error!("Failed to send CMGR command");
                return Ok(());
            }
            
            match rx.await {
                Ok(response) => {
                    if response.success {
                        if let Some(data) = response.data {
                            // Parse SMS content
                            // Response format:
                            // +CMGR: "REC UNREAD","+86138...",,"24/10/24,15:30:00+32"
                            // Content line
                            // OK
                            
                            // Simple parsing (needs robustness)
                            notifications.notify("SMS", &data, NotificationType::SMS).await;
                            
                            // Delete SMS after reading to save space
                            let del_cmd = format!("AT+CMGD={}", index);
                            let (del_tx, del_rx) = oneshot::channel();
                            let _ = cmd_tx.send((del_cmd, del_tx)).await;
                            let _ = del_rx.await;
                        }
                    }
                }
                Err(e) => error!("Failed to receive CMGR response: {}", e),
            }
        }
        Ok(())
    }
}

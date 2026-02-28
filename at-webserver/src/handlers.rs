use crate::models::CommandSender;
use crate::notifications::{NotificationManager, NotificationType};
use crate::pdu::{read_incoming_sms, SmsData};
use anyhow::Result;
use async_trait::async_trait;
use log::{debug, error, info, warn};
use regex::Regex;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

// Global regex instances
static RE_CLIP: OnceLock<Regex> = OnceLock::new();
static RE_CMTI: OnceLock<Regex> = OnceLock::new();
static RE_CMGR: OnceLock<Regex> = OnceLock::new();
static RE_PDCP: OnceLock<Regex> = OnceLock::new();
static RE_MONSC_NR: OnceLock<Regex> = OnceLock::new();
static RE_MONSC_LTE: OnceLock<Regex> = OnceLock::new();

#[async_trait]
pub trait MessageHandler: Send + Sync {
    fn can_handle(&self, line: &str) -> bool;
    async fn handle(
        &self,
        line: &str,
        notifications: &NotificationManager,
        cmd_tx: &CommandSender,
    ) -> Result<()>;
}

pub struct CallHandler;
#[async_trait]
impl MessageHandler for CallHandler {
    fn can_handle(&self, line: &str) -> bool {
        line.contains("RING") || line.contains("+CLIP:")
    }
    async fn handle(
        &self,
        line: &str,
        notifications: &NotificationManager,
        _cmd_tx: &CommandSender,
    ) -> Result<()> {
        if line.contains("RING") {
            notifications
                .notify("System", "Incoming Call (Ring)", NotificationType::Call)
                .await;
            
            if let Some(tx) = crate::server::WS_BROADCASTER.get() {
                let msg = serde_json::json!({
                    "type": "incoming_call",
                    "data": {
                        "number": "Unknown",
                        "status": "RING"
                    }
                }).to_string();
                let _ = tx.send(msg);
            }
        } else if line.contains("+CLIP:") {
            let re = RE_CLIP.get_or_init(|| Regex::new(r#"\+CLIP: "([^"]+)""#).unwrap());
            if let Some(caps) = re.captures(line) {
                if let Some(number) = caps.get(1) {
                    notifications
                        .notify(number.as_str(), "Incoming Call", NotificationType::Call)
                        .await;
                    
                    if let Some(tx) = crate::server::WS_BROADCASTER.get() {
                        let msg = serde_json::json!({
                            "type": "incoming_call",
                            "data": {
                                "number": number.as_str(),
                                "status": "CLIP"
                            }
                        }).to_string();
                        let _ = tx.send(msg);
                    }
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
    async fn handle(
        &self,
        _line: &str,
        notifications: &NotificationManager,
        _cmd_tx: &CommandSender,
    ) -> Result<()> {
        notifications
            .notify("System", "SMS Memory Full", NotificationType::MemoryFull)
            .await;
        Ok(())
    }
}

// Global cache for partial SMS parts
// Key: "sender_reference", Value: (parts_count, map<part_number, content>, timestamp)
type PartialSmsCache = Arc<Mutex<HashMap<String, (u8, HashMap<u8, String>, u64)>>>;

static PARTIAL_SMS_CACHE: OnceLock<PartialSmsCache> = OnceLock::new();

fn get_partial_cache() -> PartialSmsCache {
    PARTIAL_SMS_CACHE
        .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .clone()
}

pub struct NewSMSHandler;
#[async_trait]
impl MessageHandler for NewSMSHandler {
    fn can_handle(&self, line: &str) -> bool {
        line.contains("+CMTI:")
    }
    async fn handle(
        &self,
        line: &str,
        notifications: &NotificationManager,
        cmd_tx: &CommandSender,
    ) -> Result<()> {
        // +CMTI: "SM", 5
        let re = RE_CMTI.get_or_init(|| Regex::new(r#"\+CMTI: "(\w+)",\s*(\d+)"#).unwrap());
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
                            // Try to parse PDU from response
                            // Response might be:
                            // +CMGR: 0,,28\r\n0891683108501305F0040D916831...
                            // We need to find the PDU (hex string)
                            
                            // Find the last long hex string which is likely the PDU
                            // Or split by newline and find the line that looks like PDU
                            let lines: Vec<&str> = data.lines().collect();
                            let mut pdu_hex = "";
                            for line in lines.iter().rev() {
                                let clean_line = line.trim();
                                if clean_line.len() > 10 && clean_line.chars().all(|c| c.is_ascii_hexdigit()) {
                                    pdu_hex = clean_line;
                                    break;
                                }
                            }

                            if !pdu_hex.is_empty() {
                                match read_incoming_sms(pdu_hex) {
                                    Ok(sms_data) => {
                                        self.process_sms(sms_data, notifications).await;
                                    }
                                    Err(e) => {
                                        error!("Failed to decode PDU: {}", e);
                                        // Fallback raw notification
                                        notifications
                                            .notify("Unknown", &format!("Raw PDU: {}", pdu_hex), NotificationType::SMS)
                                            .await;
                                    }
                                }
                            } else {
                                warn!("No PDU found in CMGR response");
                            }

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

impl NewSMSHandler {
    async fn process_sms(&self, sms: SmsData, notifications: &NotificationManager) {
        if let Some(partial) = sms.partial_info {
            // Handle partial SMS
            let cache = get_partial_cache();
            let key = format!("{}_{}", sms.sender, partial.reference);
            let mut full_content = None;
            let current_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            {
                let mut map = cache.lock().unwrap();
                
                // Cleanup old entries (older than 1 hour)
                map.retain(|_, (_, _, ts)| current_time - *ts < 3600);

                let entry = map.entry(key.clone()).or_insert((partial.parts_count, HashMap::new(), current_time));
                entry.1.insert(partial.part_number, sms.content.clone());

                if entry.1.len() == entry.0 as usize {
                    // All parts received
                    let mut content = String::new();
                    for i in 1..=entry.0 {
                        if let Some(part) = entry.1.get(&i) {
                            content.push_str(part);
                        }
                    }
                    full_content = Some(content);
                }
            }

            if let Some(content) = full_content {
                {
                    let mut map = cache.lock().unwrap();
                    map.remove(&key);
                }
                info!("Combined partial SMS from {}", sms.sender);
                notifications.notify(&sms.sender, &content, NotificationType::SMS).await;
                
                if let Some(tx) = crate::server::WS_BROADCASTER.get() {
                    let msg = serde_json::json!({
                        "type": "new_sms",
                        "data": {
                            "sender": sms.sender,
                            "content": content,
                            "time": sms.timestamp,
                            "isComplete": true
                        }
                    }).to_string();
                    let _ = tx.send(msg);
                }
            } else {
                info!("Received part {}/{} from {}", partial.part_number, partial.parts_count, sms.sender);
            }
        } else {
            // Normal SMS
            notifications.notify(&sms.sender, &sms.content, NotificationType::SMS).await;
            
            if let Some(tx) = crate::server::WS_BROADCASTER.get() {
                let msg = serde_json::json!({
                    "type": "new_sms",
                    "data": {
                        "sender": sms.sender,
                        "content": sms.content,
                        "time": sms.timestamp,
                        "isComplete": true
                    }
                }).to_string();
                let _ = tx.send(msg);
            }
        }
    }
}

pub struct PDCPDataHandler;
#[async_trait]
impl MessageHandler for PDCPDataHandler {
    fn can_handle(&self, line: &str) -> bool {
        line.starts_with("^PDCPDATAINFO:")
    }
    async fn handle(
        &self,
        line: &str,
        _notifications: &NotificationManager,
        _cmd_tx: &CommandSender,
    ) -> Result<()> {
        // ^PDCPDATAINFO: 1,1,100,20,5,30,10,5,100,50,1024,2048,0,0
        let re = RE_PDCP.get_or_init(|| Regex::new(r"\^PDCPDATAINFO:(.*)").unwrap());
        if let Some(caps) = re.captures(line) {
            if let Some(data_str) = caps.get(1) {
                let parts: Vec<&str> = data_str.as_str().split(',').map(|s| s.trim()).collect();
                if parts.len() >= 14 {
                    let data = json!({
                        "type": "pdcp_data",
                        "data": {
                            "id": parts[0].parse::<i32>().unwrap_or(0),
                            "pduSessionId": parts[1].parse::<i32>().unwrap_or(0),
                            "discardTimerLen": parts[2].parse::<i32>().unwrap_or(0),
                            "avgDelay": parts[3].parse::<f64>().unwrap_or(0.0) / 10.0,
                            "minDelay": parts[4].parse::<f64>().unwrap_or(0.0) / 10.0,
                            "maxDelay": parts[5].parse::<f64>().unwrap_or(0.0) / 10.0,
                            "highPriQueMaxBuffTime": parts[6].parse::<f64>().unwrap_or(0.0) / 10.0,
                            "lowPriQueMaxBuffTime": parts[7].parse::<f64>().unwrap_or(0.0) / 10.0,
                            "highPriQueBuffPktNums": parts[8].parse::<i32>().unwrap_or(0),
                            "lowPriQueBuffPktNums": parts[9].parse::<i32>().unwrap_or(0),
                            "ulPdcpRate": parts[10].parse::<i64>().unwrap_or(0),
                            "dlPdcpRate": parts[11].parse::<i64>().unwrap_or(0),
                            "ulDiscardCnt": parts[12].parse::<i32>().unwrap_or(0),
                            "dlDiscardCnt": parts[13].parse::<i32>().unwrap_or(0),
                        }
                    });
                    
                    // Broadcast via WebSocket
                    debug!("PDCP Data: {}", data);
                    if let Some(tx) = crate::server::WS_BROADCASTER.get() {
                        let _ = tx.send(data.to_string());
                    }
                }
            }
        }
        Ok(())
    }
}

pub struct NetworkSignalHandler;
#[async_trait]
impl MessageHandler for NetworkSignalHandler {
    fn can_handle(&self, line: &str) -> bool {
        line.contains("^CERSSI:") || line.contains("^HCSQ:")
    }
    async fn handle(
        &self,
        _line: &str,
        notifications: &NotificationManager,
        cmd_tx: &CommandSender,
    ) -> Result<()> {
        // When signal changes, query detailed info
        // Simple throttling could be added here
        
        let cmd = "AT^MONSC".to_string();
        let (tx, rx) = oneshot::channel();
        if let Err(_) = cmd_tx.send((cmd, tx)).await {
            return Ok(());
        }

        if let Ok(response) = rx.await {
            if let Some(data) = response.data {
                // Parse MONSC data
                // ^MONSC: NR,51,633984,321,114,-85,-11,15,20,30
                // ^MONSC: LTE,1,1300,210,123,-90,-10,20
                
                let mut message = String::new();
                let mut _rat = "";
                let mut rsrp = 0;
                
                let re_nr = RE_MONSC_NR.get_or_init(|| 
                    Regex::new(r"\^MONSC: NR,(\d+),(\d+),(\d+),(\d+),(-?\d+),(-?\d+),(-?\d+)").unwrap()
                );
                
                let re_lte = RE_MONSC_LTE.get_or_init(||
                    Regex::new(r"\^MONSC: LTE,(\d+),(\d+),(\d+),(\d+),(-?\d+),(-?\d+),(-?\d+)").unwrap()
                );

                if let Some(caps) = re_nr.captures(&data) {
                    _rat = "NR";
                    let arfcn = caps.get(2).map_or("", |m| m.as_str());
                    let pci = caps.get(3).map_or("", |m| m.as_str());
                    rsrp = caps.get(5).map_or(0, |m| m.as_str().parse().unwrap_or(0));
                    let rsrq = caps.get(6).map_or(0, |m| m.as_str().parse().unwrap_or(0));
                    let sinr = caps.get(7).map_or(0, |m| m.as_str().parse().unwrap_or(0));
                    
                    message = format!(
                        "📶 5G Signal Info\nRAT: NR\nARFCN: {}\nPCI: {}\nRSRP: {} dBm\nRSRQ: {} dB\nSINR: {} dB",
                        arfcn, pci, rsrp, rsrq, sinr
                    );
                } else if let Some(caps) = re_lte.captures(&data) {
                    _rat = "LTE";
                    let arfcn = caps.get(2).map_or("", |m| m.as_str());
                    let pci = caps.get(3).map_or("", |m| m.as_str());
                    rsrp = caps.get(5).map_or(0, |m| m.as_str().parse().unwrap_or(0));
                    let rsrq = caps.get(6).map_or(0, |m| m.as_str().parse().unwrap_or(0));
                    let rssi = caps.get(7).map_or(0, |m| m.as_str().parse().unwrap_or(0));

                    message = format!(
                        "📶 4G Signal Info\nRAT: LTE\nARFCN: {}\nPCI: {}\nRSRP: {} dBm\nRSRQ: {} dB\nRSSI: {} dBm",
                        arfcn, pci, rsrp, rsrq, rssi
                    );
                }

                if !message.is_empty() {
                    // TODO: Logic to check thresholds and previous state to avoid spam
                    // For now, we assume this handler is called only on significant changes or periodically
                    // In a real implementation, we would need state persistence like `last_signal_data`
                    
                    // Only notify if signal is very poor or excellent as an example
                    if rsrp < -110 || rsrp > -60 {
                         notifications.notify("Signal Monitor", &message, NotificationType::Signal).await;
                    }
                }
            }
        }
        Ok(())
    }
}

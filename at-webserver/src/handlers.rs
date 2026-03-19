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

pub struct NewSMSHandler {
    delete_after_forward: bool,
}

impl NewSMSHandler {
    pub fn new(delete_after_forward: bool) -> Self {
        Self { delete_after_forward }
    }
}

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
                                        // Process SMS (notify & websocket broadcast)
                                        let forwarded = self.process_sms(sms_data, notifications).await;

                                        // 每次新短信到达时检查存储使用率
                                        Self::check_sms_storage(notifications, cmd_tx).await;
                                        
                                        // Only delete if enabled in config AND it was actually forwarded to a 3rd party service
                                        if self.delete_after_forward && forwarded {
                                            info!("Deleting SMS at index {} (forwarded & configured to auto-delete)", index);
                                            let del_cmd = format!("AT+CMGD={}", index);
                                            let (del_tx, del_rx) = oneshot::channel();
                                            let _ = cmd_tx.send((del_cmd, del_tx)).await;
                                            let _ = del_rx.await;
                                        } else {
                                            info!("Keeping SMS at index {} (auto-delete disabled or not forwarded)", index);
                                        }
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
    /// Returns true if the SMS was successfully forwarded to a third-party notification service
    async fn process_sms(&self, sms: SmsData, notifications: &NotificationManager) -> bool {
        let mut forwarded_to_third_party = false;

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
                
                // 核心逻辑：调用 notify 并检查返回值（虽然目前 notify 总是返回 void，我们需要修改 NotificationManager 以返回状态）
                // 暂时假设 NotificationManager::notify 总是成功触发配置的服务。
                // 实际上我们需要知道是否 *开启了* 任何推送服务。
                // 如果用户没有配置任何推送服务（如微信、钉钉等），那么我们不应该删除短信。
                // 但是 notify 方法内部处理了所有逻辑。
                // 为了简单起见，我们认为只要调用了 notify 就算 "尝试转发"。
                // 如果要更精确，需要修改 NotificationManager::notify 返回是否有实际推送。
                // 这里我们先调用，然后假设如果配置了服务就会推送。
                
                notifications.notify(&sms.sender, &content, NotificationType::SMS).await;
                
                // 检查是否配置了任何推送服务
                if notifications.has_active_push_services() {
                    forwarded_to_third_party = true;
                }
                
                if let Some(tx) = crate::server::WS_BROADCASTER.get() {
                    let msg = serde_json::json!({
                        "type": "new_sms",
                        "data": {
                            "sender": sms.sender,
                            "content": content,
                            "time": sms.date,
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
            
            if notifications.has_active_push_services() {
                forwarded_to_third_party = true;
            }
            
            if let Some(tx) = crate::server::WS_BROADCASTER.get() {
                let msg = serde_json::json!({
                    "type": "new_sms",
                    "data": {
                        "sender": sms.sender,
                        "content": sms.content,
                        "time": sms.date,
                        "isComplete": true
                    }
                }).to_string();
                let _ = tx.send(msg);
            }
        }
        
        forwarded_to_third_party
    }

    /// 查询短信存储使用率，超过阈值时发送通知
    async fn check_sms_storage(notifications: &NotificationManager, cmd_tx: &CommandSender) {
        let threshold = notifications.memory_full_threshold();
        if threshold == 0 {
            return; // 禁用
        }

        let (tx, rx) = oneshot::channel();
        if cmd_tx.send(("AT+CPMS?".to_string(), tx)).await.is_err() {
            return;
        }
        let resp = match rx.await {
            Ok(r) if r.success => r,
            _ => return,
        };
        let data = match resp.data {
            Some(d) => d,
            None => return,
        };

        // +CPMS: "SM",8,10,"SM",8,10,"SM",8,10
        // 取第一组 used/total
        let re = regex::Regex::new(r#"\+CPMS:\s*"\w+",(\d+),(\d+)"#).unwrap();
        if let Some(caps) = re.captures(&data) {
            let used: u32 = caps.get(1).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
            let total: u32 = caps.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(1);
            if total == 0 { return; }
            let pct = (used * 100 / total) as u8;
            info!("SMS storage: {}/{} ({}%)", used, total, pct);
            if pct >= threshold {
                let msg = format!("短信存储已使用 {}/{} ({}%)，超过阈值 {}%，请及时清理", used, total, pct, threshold);
                notifications.notify("短信存储", &msg, crate::notifications::NotificationType::MemoryFull).await;
            }
        }
    }
}

/// 处理 ^NDISSTAT URC，实时感知 NDIS 拨号连接状态变化
/// 参考 MT5700M-CN AT命令手册 16.2 ^NDISSTAT
/// 格式: ^NDISSTAT: [<cid>,]<stat>,[<err>],[<wx_state>],<PDP_type>
/// stat: 0=断开, 1=已连接
pub struct NdisStatHandler;

#[async_trait]
impl MessageHandler for NdisStatHandler {
    fn can_handle(&self, line: &str) -> bool {
        line.starts_with("^NDISSTAT:")
    }
    async fn handle(
        &self,
        line: &str,
        _notifications: &NotificationManager,
        _cmd_tx: &CommandSender,
    ) -> Result<()> {
        // ^NDISSTAT: 1,,,"IPV4"  or  ^NDISSTAT: 0,0,,"IPV4"
        let data = line.trim_start_matches("^NDISSTAT:").trim();
        let parts: Vec<&str> = data.splitn(4, ',').collect();

        // stat 可能在第1位（无cid前缀）或第2位（有cid前缀），通过是否能parse为数字判断
        let stat = parts.first().and_then(|s| s.trim().parse::<u8>().ok());
        let pdp_type = parts.last().map(|s| s.trim().trim_matches('"')).unwrap_or("");

        let (connected, stat_str) = match stat {
            Some(1) => (true, "connected"),
            Some(0) => (false, "disconnected"),
            _ => {
                warn!("^NDISSTAT: unrecognized format: {}", line);
                return Ok(());
            }
        };

        if connected {
            info!("^NDISSTAT: NDIS connection established ({})", pdp_type);
        } else {
            let err_code = parts.get(1).map(|s| s.trim()).unwrap_or("0");
            warn!("^NDISSTAT: NDIS connection lost (err={}, type={})", err_code, pdp_type);
            // 立即通知 dial_monitor 触发恢复，无需等待下一次轮询
            let tx = crate::models::get_ndis_disconnect_tx();
            let _ = tx.send(());
        }

        // 广播给前端 WebSocket
        if let Some(tx) = crate::server::WS_BROADCASTER.get() {
            let msg = serde_json::json!({
                "type": "ndis_stat",
                "data": {
                    "connected": connected,
                    "status": stat_str,
                    "pdp_type": pdp_type,
                }
            }).to_string();
            let _ = tx.send(msg);
        }
        Ok(())
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

pub struct NetworkSignalHandler {
    state: Mutex<SignalState>,
}

struct SignalState {
    last_rsrp: Option<i32>,
    last_sys_mode: Option<String>,
}

impl NetworkSignalHandler {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(SignalState {
                last_rsrp: None,
                last_sys_mode: None,
            }),
        }
    }
}

#[async_trait]
impl MessageHandler for NetworkSignalHandler {
    fn can_handle(&self, line: &str) -> bool {
        line.contains("^CERSSI:") || line.contains("^HCSQ:")
    }
    async fn handle(
        &self,
        line: &str,
        notifications: &NotificationManager,
        cmd_tx: &CommandSender,
    ) -> Result<()> {
        let line = line.trim();
        let mut current_rsrp = None;
        let mut current_sys_mode = None;

        // Parse initial signal info to decide if we need to query MONSC
        if line.contains("^CERSSI:") {
            // ^CERSSI: <...>,<rsrp>,<rsrq>,<sinr>
            // Example parts length check based on Python logic
            let replaced = line.replace("^CERSSI:", "");
            let parts: Vec<&str> = replaced.split(',').map(|s| s.trim()).collect();
            if parts.len() >= 19 {
                if let Ok(rsrp) = parts[18].parse::<i32>() {
                    current_rsrp = Some(rsrp);
                    current_sys_mode = Some("4G/5G".to_string());
                }
            }
        } else if line.contains("^HCSQ:") {
            // ^HCSQ: "LTE",<rsrp>,<sinr>,<rsrq>
            let replaced = line.replace("^HCSQ:", "");
            let parts: Vec<&str> = replaced.split(',').map(|s| s.trim()).collect();
            if parts.len() >= 4 {
                let mode = parts[0].trim_matches('"').to_string();
                if let Ok(rsrp_raw) = parts[1].parse::<i32>() {
                    // Python: rsrp = -140 + rsrp_raw
                    current_rsrp = Some(-140 + rsrp_raw);
                    current_sys_mode = Some(mode);
                }
            }
        }

        let mut should_notify = false;
        {
            let mut state = self.state.lock().unwrap();
            
            // Check if system mode changed
            if current_sys_mode != state.last_sys_mode {
                should_notify = true;
            }
            
            // Check if RSRP changed significantly (threshold = 3 dBm to be less spammy than Python's 1)
            if let (Some(curr), Some(last)) = (current_rsrp, state.last_rsrp) {
                if (curr - last).abs() >= 3 {
                    should_notify = true;
                }
            } else if current_rsrp.is_some() {
                should_notify = true;
            }

            if should_notify {
                state.last_rsrp = current_rsrp;
                state.last_sys_mode = current_sys_mode.clone();
            }
        }

        if should_notify {
            // Query detailed info
            let cmd = "AT^MONSC".to_string();
            let (tx, rx) = oneshot::channel();
            if let Err(_) = cmd_tx.send((cmd, tx)).await {
                return Ok(());
            }

            if let Ok(response) = rx.await {
                if let Some(data) = response.data {
                    let mut message = String::new();
                    
                    let re_nr = RE_MONSC_NR.get_or_init(|| 
                        Regex::new(r"\^MONSC: NR,(\d+),(\d+),(\d+),(\d+),(-?\d+),(-?\d+),(-?\d+)").unwrap()
                    );
                    
                    let re_lte = RE_MONSC_LTE.get_or_init(||
                        Regex::new(r"\^MONSC: LTE,(\d+),(\d+),(\d+),(\d+),(-?\d+),(-?\d+),(-?\d+)").unwrap()
                    );

                    if let Some(caps) = re_nr.captures(&data) {
                        let arfcn = caps.get(2).map_or("", |m| m.as_str());
                        let pci = caps.get(3).map_or("", |m| m.as_str());
                        let rsrp = caps.get(5).map_or(0, |m| m.as_str().parse().unwrap_or(0));
                        let rsrq = caps.get(6).map_or(0, |m| m.as_str().parse().unwrap_or(0));
                        let sinr = caps.get(7).map_or(0, |m| m.as_str().parse().unwrap_or(0));
                        
                        let level = if rsrp >= -85 { "优秀" } else if rsrp >= -95 { "良好" } else if rsrp >= -105 { "一般" } else { "较差" };

                        message = format!(
                            "📶 5G 信号变动\n时间: {}\n信号质量: {}\nRSRP: {} dBm\nRSRQ: {} dB\nSINR: {} dB\n\n📡 小区信息:\n频点: {}\nPCI: {}",
                            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                            level, rsrp, rsrq, sinr, arfcn, pci
                        );
                    } else if let Some(caps) = re_lte.captures(&data) {
                        let arfcn = caps.get(2).map_or("", |m| m.as_str());
                        let pci = caps.get(3).map_or("", |m| m.as_str());
                        let rsrp = caps.get(5).map_or(0, |m| m.as_str().parse().unwrap_or(0));
                        let rsrq = caps.get(6).map_or(0, |m| m.as_str().parse().unwrap_or(0));
                        let rssi = caps.get(7).map_or(0, |m| m.as_str().parse().unwrap_or(0));

                        let level = if rsrp >= -85 { "优秀" } else if rsrp >= -95 { "良好" } else if rsrp >= -105 { "一般" } else { "较差" };

                        message = format!(
                            "📶 4G 信号变动\n时间: {}\n信号质量: {}\nRSRP: {} dBm\nRSRQ: {} dB\nRSSI: {} dBm\n\n📡 小区信息:\n频点: {}\nPCI: {}",
                            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                            level, rsrp, rsrq, rssi, arfcn, pci
                        );
                    }

                    if !message.is_empty() {
                        notifications.notify("信号监控", &message, NotificationType::Signal).await;
                    }
                }
            }
        }
        Ok(())
    }
}

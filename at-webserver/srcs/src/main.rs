use async_trait::async_trait;
use chrono::{NaiveTime, Timelike, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::net::{IpAddr, Ipv6Addr};
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
// use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, broadcast};
use tokio::time::{Instant, interval, sleep, timeout};
use tokio_serial::{SerialPortBuilderExt, SerialStream};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use chrono_tz::Asia::Shanghai;

// 添加自定义错误类型
#[derive(Debug)]
struct StringError(String);

impl std::fmt::Display for StringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for StringError {}

const DEFAULT_CONFIG_JSON: &str = r#"{
    "AT_CONFIG": {
        "TYPE": "NETWORK",
        "NETWORK": { "HOST": "192.168.8.1", "PORT": 20249, "TIMEOUT": 30 },
        "SERIAL": { 
            "PORT": "COM6", 
            "BAUDRATE": 115200, 
            "TIMEOUT": 30,
            "METHOD": "UBUS",
            "FEATURE": "NONE"
        }
    },
    "WEBSOCKET_CONFIG": {
        "IPV4": { "HOST": "0.0.0.0", "PORT": 8765 },
        "IPV6": { "HOST": "::", "PORT": 8765 },
        "AUTH_KEY": ""
    },
    "NOTIFICATION_CONFIG": {
        "WECHAT_WEBHOOK": "",
        "LOG_FILE": "",
        "NOTIFICATION_TYPES": {
            "SMS": true,
            "CALL": true,
            "MEMORY_FULL": true,
            "SIGNAL": true
        }
    },
    "SCHEDULE_AIRPLANE_CONFIG": {
        "ENABLED": false,
        "ACTION_TIME": "8:00"
    },
    "SCHEDULE_CONFIG": {
        "ENABLED": false,
        "CHECK_INTERVAL": 60,
        "TIMEOUT": 180,
        "UNLOCK_LTE": true,
        "UNLOCK_NR": true,
        "TOGGLE_AIRPLANE": true,
        "NIGHT_ENABLED": true,
        "NIGHT_START": "22:00",
        "NIGHT_END": "06:00",
        "NIGHT_LTE_TYPE": 0,
        "NIGHT_LTE_BANDS": "",
        "NIGHT_LTE_ARFCNS": "",
        "NIGHT_LTE_PCIS": "",
        "NIGHT_NR_TYPE": 0,
        "NIGHT_NR_BANDS": "",
        "NIGHT_NR_ARFCNS": "",
        "NIGHT_NR_SCS_TYPES": "",
        "NIGHT_NR_PCIS": "",
        "DAY_ENABLED": true,
        "DAY_LTE_TYPE": 0,
        "DAY_LTE_BANDS": "",
        "DAY_LTE_ARFCNS": "",
        "DAY_LTE_PCIS": "",
        "DAY_NR_TYPE": 0,
        "DAY_NR_BANDS": "",
        "DAY_NR_ARFCNS": "",
        "DAY_NR_SCS_TYPES": "",
        "DAY_NR_PCIS": ""
    }
}"#;

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Config {
    #[serde(rename = "AT_CONFIG")]
    at_config: AtConfig,
    #[serde(rename = "WEBSOCKET_CONFIG")]
    websocket_config: WsConfig,
    #[serde(rename = "NOTIFICATION_CONFIG")]
    notification_config: NotificationConfig,
    #[serde(rename = "SCHEDULE_AIRPLANE_CONFIG")]
    auto_airplane: AutoAirPlane,
    #[serde(rename = "SCHEDULE_CONFIG")]
    schedule_config: ScheduleConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct AtConfig {
    #[serde(rename = "TYPE")]
    conn_type: String,
    #[serde(rename = "NETWORK")]
    network: NetworkConfig,
    #[serde(rename = "SERIAL")]
    serial: SerialConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct NetworkConfig {
    #[serde(rename = "HOST")]
    host: String,
    #[serde(rename = "PORT")]
    port: u16,
    #[serde(rename = "TIMEOUT")]
    timeout: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct SerialConfig {
    #[serde(rename = "PORT")]
    port: String,
    #[serde(rename = "BAUDRATE")]
    baudrate: u32,
    #[serde(rename = "TIMEOUT")]
    timeout: u64,
    #[serde(rename = "METHOD")]
    method: String,
    #[serde(rename = "FEATURE")]
    feature: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct WsConfig {
    #[serde(rename = "IPV4")]
    ipv4: WsEndpoint,
    #[serde(rename = "IPV6")]
    ipv6: WsEndpoint,
    #[serde(rename = "AUTH_KEY")]
    auth_key: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct WsEndpoint {
    #[serde(rename = "HOST")]
    host: String,
    #[serde(rename = "PORT")]
    port: u16,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct NotificationConfig {
    #[serde(rename = "WECHAT_WEBHOOK")]
    wechat_webhook: String,
    #[serde(rename = "LOG_FILE")]
    log_file: String,
    #[serde(rename = "NOTIFICATION_TYPES")]
    notification_types: NotificationTypes,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct NotificationTypes {
    #[serde(rename = "SMS")]
    sms: bool,
    #[serde(rename = "CALL")]
    call: bool,
    #[serde(rename = "MEMORY_FULL")]
    memory_full: bool,
    #[serde(rename = "SIGNAL")]
    signal: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct AutoAirPlane {
    #[serde(rename = "ENABLED")]
    enabled: bool,
    #[serde(rename = "ACTION_TIME")]
    action_time: String,
}


#[derive(Debug, Serialize, Deserialize, Clone)]
struct ScheduleConfig {
    #[serde(rename = "ENABLED")]
    enabled: bool,
    #[serde(rename = "CHECK_INTERVAL")]
    check_interval: u64,
    #[serde(rename = "TIMEOUT")]
    timeout: u64,
    #[serde(rename = "UNLOCK_LTE")]
    unlock_lte: bool,
    #[serde(rename = "UNLOCK_NR")]
    unlock_nr: bool,
    #[serde(rename = "TOGGLE_AIRPLANE")]
    toggle_airplane: bool,
    #[serde(rename = "NIGHT_ENABLED")]
    night_enabled: bool,
    #[serde(rename = "NIGHT_START")]
    night_start: String,
    #[serde(rename = "NIGHT_END")]
    night_end: String,
    #[serde(rename = "NIGHT_LTE_TYPE")]
    night_lte_type: u8,
    #[serde(rename = "NIGHT_LTE_BANDS")]
    night_lte_bands: String,
    #[serde(rename = "NIGHT_LTE_ARFCNS")]
    night_lte_arfcns: String,
    #[serde(rename = "NIGHT_LTE_PCIS")]
    night_lte_pcis: String,
    #[serde(rename = "NIGHT_NR_TYPE")]
    night_nr_type: u8,
    #[serde(rename = "NIGHT_NR_BANDS")]
    night_nr_bands: String,
    #[serde(rename = "NIGHT_NR_ARFCNS")]
    night_nr_arfcns: String,
    #[serde(rename = "NIGHT_NR_SCS_TYPES")]
    night_nr_scs_types: String,
    #[serde(rename = "NIGHT_NR_PCIS")]
    night_nr_pcis: String,
    #[serde(rename = "DAY_ENABLED")]
    day_enabled: bool,
    #[serde(rename = "DAY_LTE_TYPE")]
    day_lte_type: u8,
    #[serde(rename = "DAY_LTE_BANDS")]
    day_lte_bands: String,
    #[serde(rename = "DAY_LTE_ARFCNS")]
    day_lte_arfcns: String,
    #[serde(rename = "DAY_LTE_PCIS")]
    day_lte_pcis: String,
    #[serde(rename = "DAY_NR_TYPE")]
    day_nr_type: u8,
    #[serde(rename = "DAY_NR_BANDS")]
    day_nr_bands: String,
    #[serde(rename = "DAY_NR_ARFCNS")]
    day_nr_arfcns: String,
    #[serde(rename = "DAY_NR_SCS_TYPES")]
    day_nr_scs_types: String,
    #[serde(rename = "DAY_NR_PCIS")]
    day_nr_pcis: String,
}

fn load_config_from_uci() -> Result<Config, Box<dyn Error>> {
    println!("开始从 UCI 加载配置...");

    // 执行 uci 命令
    let output = Command::new("uci")
        .args(&["show", "at-webserver"])
        .output()?;

    if !output.status.success() {
        println!("读取 UCI 配置失败，使用默认配置");
        return serde_json::from_str(DEFAULT_CONFIG_JSON)
            .map_err(|e| format!("解析默认配置失败: {}", e).into());
    }

    let output_str = String::from_utf8_lossy(&output.stdout);
    let mut uci_data = HashMap::new();

    // 解析 UCI 输出
    for line in output_str.trim().lines() {
        if line.contains('=') {
            let parts: Vec<&str> = line.splitn(2, '=').collect();
            if parts.len() == 2 {
                let key = parts[0];
                let value = parts[1].trim_matches(|c| c == '\'' || c == '"');

                // 移除前缀 'at-webserver.config.'
                if key.starts_with("at-webserver.config.") {
                    let short_key = key.replace("at-webserver.config.", "");
                    uci_data.insert(short_key, value.to_string());
                }
            }
        }
    }
   
    // 从默认配置开始
    println!("使用默认配置初始化...");
    let mut config: Config = serde_json::from_str(DEFAULT_CONFIG_JSON)?;

    println!("开始从 UCI 加载配置...");
    // 读取连接类型
    let conn_type = uci_data
        .get("connection_type")
        .map(|s| s.as_str())
        .unwrap_or("NETWORK");
    config.at_config.conn_type = conn_type.to_string();
    println!("配置加载: 连接类型 = {}", conn_type);

    // 读取网络配置
    if conn_type == "NETWORK" {
        let host = uci_data
            .get("network_host")
            .map(|s| s.as_str())
            .unwrap_or("192.168.8.1");
        let port = uci_data
            .get("network_port")
            .map(|s| s.parse().unwrap_or(20249))
            .unwrap_or(20249);
        let timeout = uci_data
            .get("network_timeout")
            .map(|s| s.parse().unwrap_or(10))
            .unwrap_or(10);

        config.at_config.network.host = host.to_string();
        config.at_config.network.port = port;
        config.at_config.network.timeout = timeout;
        println!("配置加载: 网络连接 {}:{} (超时: {}秒)", host, port, timeout);
    } else {
        // 读取串口配置
        let mut port = uci_data
            .get("serial_port")
            .map(|s| s.as_str())
            .unwrap_or("/dev/ttyUSB0")
            .to_string();

        // 如果选择了自定义路径，读取自定义值
        if port == "custom" {
            port = uci_data
                .get("serial_port_custom")
                .map(|s| s.as_str())
                .unwrap_or("/dev/ttyUSB0")
                .to_string();
        }

        let baudrate = uci_data
            .get("serial_baudrate")
            .map(|s| s.parse().unwrap_or(115200))
            .unwrap_or(115200);
        let timeout = uci_data
            .get("serial_timeout")
            .map(|s| s.parse().unwrap_or(10))
            .unwrap_or(10);

        config.at_config.serial.port = port.clone();
        config.at_config.serial.baudrate = baudrate;
        config.at_config.serial.timeout = timeout;

        // 读取串口方法和功能
        // 优先读取 use_ubus (新配置)，如果不存在则回退到 serial_method (旧配置)
        let method = if let Some(use_ubus) = uci_data.get("use_ubus") {
            if use_ubus == "1" {
                "UBUS"
            } else {
                "DIRECT"
            }
        } else {
            uci_data
                .get("serial_method")
                .map(|s| s.as_str())
                .unwrap_or("UBUS")
        };
        
        let feature = uci_data
            .get("serial_feature")
            .map(|s| s.as_str())
            .unwrap_or("NONE");

        config.at_config.serial.method = method.to_string();
        config.at_config.serial.feature = feature.to_string();

        println!(
            "配置加载: 串口连接 {} @ {} bps (超时: {}秒)",
            port, baudrate, timeout
        );
        println!("配置加载: 串口方法 = {}, 功能 = {}", method, feature);
    }

    // 读取 WebSocket 端口
    let ws_port = uci_data
        .get("websocket_port")
        .map(|s| s.parse().unwrap_or(8765))
        .unwrap_or(8765);
    config.websocket_config.ipv4.port = ws_port;
    config.websocket_config.ipv6.port = ws_port;

    // 读取是否允许外网访问
    let allow_wan = uci_data
        .get("websocket_allow_wan")
        .map(|s| s == "1")
        .unwrap_or(false);

    // WebSocket 始终监听所有网卡
    config.websocket_config.ipv4.host = "0.0.0.0".to_string();
    config.websocket_config.ipv6.host = "::".to_string();

    // 读取连接密钥
    let auth_key = uci_data
        .get("websocket_auth_key")
        .map(|s| s.as_str())
        .unwrap_or("");
    config.websocket_config.auth_key = auth_key.to_string();

    if allow_wan {
        println!("配置加载: WebSocket 端口 = {} (允许外网访问)", ws_port);
        println!("⚠ 外网访问已启用，请确保已配置防火墙规则保护");
    } else {
        println!("配置加载: WebSocket 端口 = {} (局域网访问)", ws_port);
        println!("💡 如需限制访问，建议配置防火墙规则");
    }

    if !auth_key.is_empty() {
        println!("配置加载: 连接密钥已设置 (长度: {})", auth_key.len());
    } else {
        println!("配置加载: 连接密钥未设置 (允许无密钥访问)");
    }

    // 读取通知配置
    if let Some(wechat_webhook) = uci_data.get("wechat_webhook") {
        config.notification_config.wechat_webhook = wechat_webhook.clone();
        println!("配置加载: 企业微信推送已启用");
    }

    if let Some(log_file) = uci_data.get("log_file") {
        config.notification_config.log_file = log_file.clone();
        println!("配置加载: 日志文件 = {}", log_file);
    }

    // 读取通知类型开关
    if let Some(notify_sms) = uci_data.get("notify_sms") {
        config.notification_config.notification_types.sms = notify_sms == "1";
    }
    if let Some(notify_call) = uci_data.get("notify_call") {
        config.notification_config.notification_types.call = notify_call == "1";
    }
    if let Some(notify_memory_full) = uci_data.get("notify_memory_full") {
        config.notification_config.notification_types.memory_full = notify_memory_full == "1";
    }
    if let Some(notify_signal) = uci_data.get("notify_signal") {
        config.notification_config.notification_types.signal = notify_signal == "1";
    }

    // 读取自动开关飞行模式
    if let Some(auto_airplane) = uci_data.get("schedule_auto_airplane_enable") {
        let enabled = auto_airplane == "1";
        let action_time = uci_data
            .get("schedule_airplane_time")
            .map(|s: &String| s.as_str())
            .unwrap_or("8:00")
            .to_string();

        println!(
            "配置加载: 自动开关飞行模式 = {} (时间: {})",
            if enabled { "启用" } else { "禁用" },
            action_time
        );

        // 这里假设 Config 结构体中有一个 auto_airplane 字段
        // 你需要在 Config 结构体中添加相应的字段
        config.auto_airplane.enabled = enabled;
        config.auto_airplane.action_time = action_time;
    }

    /* 暂不启用定时锁频监控 
    // 读取定时锁频配置
    if let Some(schedule_enabled) = uci_data.get("schedule_enabled") {
        config.schedule_config.enabled = schedule_enabled == "1";
    }

    if let Some(check_interval) = uci_data.get("schedule_check_interval") {
        config.schedule_config.check_interval = check_interval.parse().unwrap_or(60);
    }

    if let Some(schedule_timeout) = uci_data.get("schedule_timeout") {
        config.schedule_config.timeout = schedule_timeout.parse().unwrap_or(180);
    }

    if let Some(unlock_lte) = uci_data.get("schedule_unlock_lte") {
        config.schedule_config.unlock_lte = unlock_lte == "1";
    }

    if let Some(unlock_nr) = uci_data.get("schedule_unlock_nr") {
        config.schedule_config.unlock_nr = unlock_nr == "1";
    }

    if let Some(toggle_airplane) = uci_data.get("schedule_toggle_airplane") {
        config.schedule_config.toggle_airplane = toggle_airplane == "1";
    }

    // 夜间模式配置
    if let Some(night_enabled) = uci_data.get("schedule_night_enabled") {
        config.schedule_config.night_enabled = night_enabled == "1";
    }

    if let Some(night_start) = uci_data.get("schedule_night_start") {
        config.schedule_config.night_start = night_start.clone();
    }

    if let Some(night_end) = uci_data.get("schedule_night_end") {
        config.schedule_config.night_end = night_end.clone();
    }

    if let Some(night_lte_type) = uci_data.get("schedule_night_lte_type") {
        config.schedule_config.night_lte_type = night_lte_type.parse().unwrap_or(0);
    }

    if let Some(night_lte_bands) = uci_data.get("schedule_night_lte_bands") {
        config.schedule_config.night_lte_bands = night_lte_bands.clone();
    }

    if let Some(night_lte_arfcns) = uci_data.get("schedule_night_lte_arfcns") {
        config.schedule_config.night_lte_arfcns = night_lte_arfcns.clone();
    }

    if let Some(night_lte_pcis) = uci_data.get("schedule_night_lte_pcis") {
        config.schedule_config.night_lte_pcis = night_lte_pcis.clone();
    }

    if let Some(night_nr_type) = uci_data.get("schedule_night_nr_type") {
        config.schedule_config.night_nr_type = night_nr_type.parse().unwrap_or(0);
    }

    if let Some(night_nr_bands) = uci_data.get("schedule_night_nr_bands") {
        config.schedule_config.night_nr_bands = night_nr_bands.clone();
    }

    if let Some(night_nr_arfcns) = uci_data.get("schedule_night_nr_arfcns") {
        config.schedule_config.night_nr_arfcns = night_nr_arfcns.clone();
    }

    if let Some(night_nr_scs_types) = uci_data.get("schedule_night_nr_scs_types") {
        config.schedule_config.night_nr_scs_types = night_nr_scs_types.clone();
    }

    if let Some(night_nr_pcis) = uci_data.get("schedule_night_nr_pcis") {
        config.schedule_config.night_nr_pcis = night_nr_pcis.clone();
    }

    // 日间模式配置
    if let Some(day_enabled) = uci_data.get("schedule_day_enabled") {
        config.schedule_config.day_enabled = day_enabled == "1";
    }

    if let Some(day_lte_type) = uci_data.get("schedule_day_lte_type") {
        config.schedule_config.day_lte_type = day_lte_type.parse().unwrap_or(0);
    }

    if let Some(day_lte_bands) = uci_data.get("schedule_day_lte_bands") {
        config.schedule_config.day_lte_bands = day_lte_bands.clone();
    }

    if let Some(day_lte_arfcns) = uci_data.get("schedule_day_lte_arfcns") {
        config.schedule_config.day_lte_arfcns = day_lte_arfcns.clone();
    }

    if let Some(day_lte_pcis) = uci_data.get("schedule_day_lte_pcis") {
        config.schedule_config.day_lte_pcis = day_lte_pcis.clone();
    }

    if let Some(day_nr_type) = uci_data.get("schedule_day_nr_type") {
        config.schedule_config.day_nr_type = day_nr_type.parse().unwrap_or(0);
    }

    if let Some(day_nr_bands) = uci_data.get("schedule_day_nr_bands") {
        config.schedule_config.day_nr_bands = day_nr_bands.clone();
    }

    if let Some(day_nr_arfcns) = uci_data.get("schedule_day_nr_arfcns") {
        config.schedule_config.day_nr_arfcns = day_nr_arfcns.clone();
    }

    if let Some(day_nr_scs_types) = uci_data.get("schedule_day_nr_scs_types") {
        config.schedule_config.day_nr_scs_types = day_nr_scs_types.clone();
    }

    if let Some(day_nr_pcis) = uci_data.get("schedule_day_nr_pcis") {
        config.schedule_config.day_nr_pcis = day_nr_pcis.clone();
    }

    if config.schedule_config.enabled {
        println!(
            "配置加载: 定时锁频已启用 (检测间隔: {}秒, 超时: {}秒)",
            config.schedule_config.check_interval, config.schedule_config.timeout
        );
        println!(
            "  夜间模式: {} ({}-{})",
            if config.schedule_config.night_enabled {
                "启用"
            } else {
                "禁用"
            },
            config.schedule_config.night_start,
            config.schedule_config.night_end
        );
        println!(
            "  日间模式: {}",
            if config.schedule_config.day_enabled {
                "启用"
            } else {
                "禁用"
            }
        );
        println!(
            "  解锁LTE: {}, 解锁NR: {}, 切飞行模式: {}",
            if config.schedule_config.unlock_lte {
                "是"
            } else {
                "否"
            },
            if config.schedule_config.unlock_nr {
                "是"
            } else {
                "否"
            },
            if config.schedule_config.toggle_airplane {
                "是"
            } else {
                "否"
            }
        );
    }
    */

    println!("✓ UCI 配置加载完成");
    Ok(config)
}

#[async_trait]
trait ATConnection: Send {
    async fn connect(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>;
    async fn send(&mut self, data: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>>;
    async fn receive(&mut self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>>;
    fn is_connected(&self) -> bool;
}

struct SerialATConn {
    config: SerialConfig,
    stream: Option<SerialStream>,
}
#[async_trait]
impl ATConnection for SerialATConn {
    async fn connect(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let port = tokio_serial::new(&self.config.port, self.config.baudrate)
            .timeout(Duration::from_secs(self.config.timeout))
            .open_native_async()?;
        self.stream = Some(port);
        Ok(())
    }
    async fn send(&mut self, data: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        if let Some(s) = &mut self.stream {
            return Ok(s.write(data).await?);
        }
        Err("Disconnected".into())
    }
    async fn receive(&mut self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        if let Some(s) = &mut self.stream {
            let mut buf = vec![0u8; 1024];
            let n = timeout(Duration::from_millis(25), s.read(&mut buf)).await??;
            buf.truncate(n);
            return Ok(buf);
        }
        Err("Disconnected".into())
    }
    fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
}

struct NetworkATConn {
    config: NetworkConfig,
    stream: Option<TcpStream>,
}
#[async_trait]
impl ATConnection for NetworkATConn {
    async fn connect(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let addr = format!("{}:{}", self.config.host, self.config.port);
        let stream = timeout(
            Duration::from_secs(self.config.timeout),
            TcpStream::connect(addr),
        )
        .await??;
        self.stream = Some(stream);
        Ok(())
    }
    async fn send(&mut self, data: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        if let Some(s) = &mut self.stream {
            return Ok(s.write(data).await?);
        }
        Err("Disconnected".into())
    }
    async fn receive(&mut self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        if let Some(s) = &mut self.stream {
            let mut buf = vec![0u8; 1024];
            let n = timeout(Duration::from_millis(25), s.read(&mut buf)).await??;
            buf.truncate(n);
            return Ok(buf);
        }
        Err("Disconnected".into())
    }
    fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
}


// UbusAtDaemonConnection 实现
struct UbusAtDaemonConn {
    port: String,
    timeout: u64,
    is_connected: bool,
    response: Option<String>,
}

#[async_trait]
impl ATConnection for UbusAtDaemonConn {
    async fn connect(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.is_connected = true;
        Ok(())
    }

    async fn send(&mut self, data: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        if !self.is_connected {
            return Err("Disconnected".into());
        }

        let command = String::from_utf8_lossy(data).trim().to_string();
        
        // 构造 JSON 参数
        let params = serde_json::json!({
            "at_port": self.port,
            "at_cmd": command,
            "timeout": self.timeout
        });

        // 执行 ubus 命令
        // ubus call at-daemon sendat '{"at_port":"...","at_cmd":"..."}'
        // QModem / at-daemon 期望的参数:
        // {
        //   "at_port": "/dev/ttyUSB0",
        //   "at_cmd": "AT+CGMM",
        //   "timeout": 5
        // }
        
        let output = timeout(
            Duration::from_secs(self.timeout + 2),
            tokio::process::Command::new("ubus")
                .arg("call")
                .arg("at-daemon")
                .arg("sendat")
                .arg(params.to_string())
                .output(),
        )
        .await??;

        if output.status.success() {
            let stdout_str = String::from_utf8_lossy(&output.stdout);
            
            // at-daemon sendat 的返回格式:
            // 1. 直接通过 ubus call: 返回一个 JSON 对象
            // {
            //   "port": "/dev/ttyUSB0",
            //   "command": "at+cgmm",
            //   "status": "success",
            //   "response": "\r\nMH5000-82M\r\n\r\nOK\r\n",
            //   ...
            // }
            // 2. 如果通过 rpcd HTTP 接口: 返回 JSON-RPC 格式
            // { "result": [0, { ... }] }
            //
            // 我们这里是直接调用 ubus 命令，所以应该是情况 1。
            
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&stdout_str) {
                let response_str = if let Some(resp) = v.get("response") {
                    resp.as_str().unwrap_or("").to_string()
                } else {
                     // 兼容性处理：虽然直接调用 ubus 不太可能返回 JSON-RPC 数组，但以防万一
                     if let Some(arr) = v.as_array() {
                         if arr.len() > 1 {
                             arr[1].get("response").and_then(|r| r.as_str()).unwrap_or("").to_string()
                         } else {
                             "".to_string()
                         }
                     } else {
                        if let Some(err) = v.get("error") {
                             format!("ERROR: {}", err)
                        } else {
                             // 如果 status 是 failed，尝试获取 msg 或其他错误信息
                             if let Some(status) = v.get("status") {
                                 if status.as_str() == Some("failed") {
                                     format!("ERROR: {}", v.get("msg").and_then(|m| m.as_str()).unwrap_or("Unknown error"))
                                 } else {
                                     format!("ERROR: No 'response' field in ubus output: {}", stdout_str)
                                 }
                             } else {
                                 format!("ERROR: No 'response' field in ubus output: {}", stdout_str)
                             }
                        }
                     }
                };

                self.response = Some(response_str);
                Ok(data.len())
            } else {
                Err(format!("ubus output parse error: {}", stdout_str).into())
            }
        } else {
            let error = String::from_utf8_lossy(&output.stderr);
            Err(format!("ubus执行失败: {}", error).into())
        }
    }

    async fn receive(&mut self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        if let Some(response) = &self.response {
            let data = response.clone().into_bytes();
            self.response = None;
            Ok(data)
        } else {
            Ok(Vec::new())
        }
    }

    fn is_connected(&self) -> bool {
        self.is_connected
    }
}

struct ATClient {
    conn: Arc<Mutex<Box<dyn ATConnection>>>,
    urc_tx: broadcast::Sender<String>,
    config: Arc<Config>,
}

impl ATClient {
    fn new(config: Arc<Config>) -> Result<Self, Box<dyn Error>> {
        let at_config = &config.at_config;

        let conn: Box<dyn ATConnection> = if at_config.conn_type == "NETWORK" {
            Box::new(NetworkATConn {
                config: at_config.network.clone(),
                stream: None,
            })
        } else {
            if at_config.serial.method == "UBUS" || at_config.serial.method == "QMODEM" {
                Box::new(UbusAtDaemonConn {
                    port: at_config.serial.port.clone(),
                    timeout: at_config.serial.timeout,
                    is_connected: false,
                    response: None,
                })
            } else {
                Box::new(SerialATConn {
                    config: at_config.serial.clone(),
                    stream: None,
                })
            }
        };

        let (tx, _) = broadcast::channel(1024);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            urc_tx: tx,
            config,
        })
    }

    async fn send_command(
        &self,
        mut command: String,
    ) -> Result<String, Box<dyn Error + Send + Sync>> {
        let mut conn = self.conn.lock().await;
        let original_cmd = command.trim().to_string();
        if !command.ends_with("\r\n") {
            command = command.trim_end().to_string();
            command.push_str("\r\n");
        }

        // 1. 清理旧残留，防止 ping 干扰指令结果
        while let Ok(d) = timeout(Duration::from_millis(10), conn.receive())
            .await
            .unwrap_or(Ok(vec![]))
        {
            if d.is_empty() {
                break;
            }
        }

        println!("[DEBUG] ==> TX: {:?}", command);
        conn.send(command.as_bytes()).await?;

        let mut raw_response = String::new();
        let start = Instant::now();

        // 2. 超时设为 1000ms
        while start.elapsed() < Duration::from_millis(1000) {
            if let Ok(data) = conn.receive().await {
                if !data.is_empty() {
                    raw_response.push_str(&String::from_utf8_lossy(&data));
                    // 如果看到 OK 或 ERROR，说明指令响应结束
                    if raw_response.contains("OK\r\n") || raw_response.contains("ERROR") {
                        break;
                    }
                }
            }
            sleep(Duration::from_millis(10)).await;
        }

        let mut cleaned = raw_response.replace("ping", "").trim().to_string();
        if cleaned.trim_start().starts_with(&original_cmd) {
            if let Some(pos) = cleaned.find('\n') {
                cleaned = cleaned[(pos + 1)..].to_string();
            }
        }

        let result = cleaned.trim().to_string();
        println!("[DEBUG] <== RX: {:?}", result);

        // 如果结果包含 ERROR，返回 Err 分支
        if result.contains("ERROR") {
            return Err("ERROR".into());
        }

        if result.is_empty() && start.elapsed() >= Duration::from_millis(1000) {
            return Err("TIMEOUT".into());
        }

        Ok(result)
    }

    async fn init_module(&self) {
        let _ = self.send_command("ATE0".into()).await;
        let _ = self.send_command("AT+CNMI=2,1,0,2,0".into()).await;
        let _ = self.send_command("AT+CMGF=0".into()).await;
        let _ = self.send_command("AT+CLIP=1".into()).await;
    }
}

// =====================定时重启飞行模式功能======================
struct AutoAirPlaneMode {
    client: Arc<ATClient>,
    enabled: bool,
    action_time: String,
    action_hour: u32,
    action_minute: u32,
}

impl AutoAirPlaneMode {
    fn new(client: Arc<ATClient>, config: Arc<Config>) -> Self {
        let auto_airplane = &config.auto_airplane;
        
        // 预解析时间
        let (hour, minute) = if auto_airplane.enabled {
            let parts: Vec<&str> = auto_airplane.action_time.split(':').collect();
            if parts.len() == 2 {
                let h = parts[0].parse().unwrap_or(8);
                let m = parts[1].parse().unwrap_or(0);
                if h < 24 && m < 60 {
                    (h, m)
                } else {
                    (8, 0)
                }
            } else {
                (8, 0)
            }
        } else {
            (0, 0)
        };

        let mode = Self {
            client,
            enabled: auto_airplane.enabled,
            action_time: auto_airplane.action_time.clone(),
            action_hour: hour,
            action_minute: minute,
        };

        if mode.enabled {
            println!("{}", "=".repeat(60));
            println!("自动开关飞行模式功能已启用");
            println!("  操作时间: {}", mode.action_time);
            println!("{}", "=".repeat(60));
        }

        mode
    }

    fn is_action_time(&self, now: &chrono::DateTime<chrono_tz::Tz>) -> bool {
        now.hour() == self.action_hour && now.minute() == self.action_minute
    }

    fn current_time_string(&self) -> String {
        let now = Utc::now().with_timezone(&Shanghai);
        now.format("%H:%M").to_string()
    }

    fn restart_airplane_mode(&self) {
        let client = self.client.clone();
        tokio::spawn(async move {
            println!(
                "[{}] 自动重启飞行模式开始...",
                Utc::now().with_timezone(&Shanghai).format("%Y-%m-%d %H:%M:%S")
            );

            // 关闭飞行模式
            match client.send_command("AT+CFUN=0".into()).await {
                Ok(_) => println!("飞行模式已开启"),
                Err(e) => println!("开启飞行模式失败: {}", e),
            }

            // 等待10秒
            sleep(Duration::from_secs(10)).await;

            // 打开飞行模式
            match client.send_command("AT+CFUN=1".into()).await {
                Ok(_) => println!("飞行模式已关闭"),
                Err(e) => println!("关闭飞行模式失败: {}", e),
            }

            println!(
                "[{}] 自动重启飞行模式完成",
                Utc::now().with_timezone(&Shanghai).format("%Y-%m-%d %H:%M:%S")
            );
        });
    }

    async fn monitor_loop(self) {
        // let mode = self.clone();
        tokio::spawn(async move {
            loop {
                if self.enabled {
                    let now = Utc::now().with_timezone(&Shanghai);
                    println!("当前时间: {}", now.format("%H:%M"));

                    if self.is_action_time(&now) {
                        self.restart_airplane_mode();
                        // 等待60秒，避免在同一分钟内重复触发
                        sleep(Duration::from_secs(60)).await;
                    }
                }
                // 每分钟查询一次
                sleep(Duration::from_secs(60)).await;
            }
        });
    }
}
// ==============================================================

/*  定时锁频功能 暂不启用
// ====================== 定时锁频功能 ======================
struct ScheduleFrequencyLock {
    client: Arc<ATClient>,
    enabled: bool,
    check_interval: u64,
    timeout: u64,
    unlock_lte: bool,
    unlock_nr: bool,
    toggle_airplane: bool,
    night_enabled: bool,
    night_start: String,
    night_end: String,
    night_lte_type: u8,
    night_lte_bands: String,
    night_lte_arfcns: String,
    night_lte_pcis: String,
    night_nr_type: u8,
    night_nr_bands: String,
    night_nr_arfcns: String,
    night_nr_scs_types: String,
    night_nr_pcis: String,
    day_enabled: bool,
    day_lte_type: u8,
    day_lte_bands: String,
    day_lte_arfcns: String,
    day_lte_pcis: String,
    day_nr_type: u8,
    day_nr_bands: String,
    day_nr_arfcns: String,
    day_nr_scs_types: String,
    day_nr_pcis: String,

    last_service_time: u64,
    is_switching: bool,
    switch_count: u32,
    current_mode: Option<String>, // Some("night") 或 Some("day")
}

impl ScheduleFrequencyLock {
    fn new(client: Arc<ATClient>, config: Arc<Config>) -> Self {
        let schedule = &config.schedule_config;

        let lock = Self {
            client,
            enabled: schedule.enabled,
            check_interval: schedule.check_interval,
            timeout: schedule.timeout,
            unlock_lte: schedule.unlock_lte,
            unlock_nr: schedule.unlock_nr,
            toggle_airplane: schedule.toggle_airplane,
            night_enabled: schedule.night_enabled,
            night_start: schedule.night_start.clone(),
            night_end: schedule.night_end.clone(),
            night_lte_type: schedule.night_lte_type,
            night_lte_bands: schedule.night_lte_bands.clone(),
            night_lte_arfcns: schedule.night_lte_arfcns.clone(),
            night_lte_pcis: schedule.night_lte_pcis.clone(),
            night_nr_type: schedule.night_nr_type,
            night_nr_bands: schedule.night_nr_bands.clone(),
            night_nr_arfcns: schedule.night_nr_arfcns.clone(),
            night_nr_scs_types: schedule.night_nr_scs_types.clone(),
            night_nr_pcis: schedule.night_nr_pcis.clone(),
            day_enabled: schedule.day_enabled,
            day_lte_type: schedule.day_lte_type,
            day_lte_bands: schedule.day_lte_bands.clone(),
            day_lte_arfcns: schedule.day_lte_arfcns.clone(),
            day_lte_pcis: schedule.day_lte_pcis.clone(),
            day_nr_type: schedule.day_nr_type,
            day_nr_bands: schedule.day_nr_bands.clone(),
            day_nr_arfcns: schedule.day_nr_arfcns.clone(),
            day_nr_scs_types: schedule.day_nr_scs_types.clone(),
            day_nr_pcis: schedule.day_nr_pcis.clone(),

            last_service_time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            is_switching: false,
            switch_count: 0,
            current_mode: None,
        };

        if lock.enabled {
            println!("{}", "=".repeat(60));
            println!("定时锁频功能已启用");
            println!("  检测间隔: {} 秒", lock.check_interval);
            println!("  无服务超时: {} 秒", lock.timeout);
            println!(
                "  夜间模式: {} ({}-{})",
                if lock.night_enabled {
                    "启用"
                } else {
                    "禁用"
                },
                lock.night_start,
                lock.night_end
            );
            println!(
                "  日间模式: {}",
                if lock.day_enabled { "启用" } else { "禁用" }
            );
            println!(
                "  解锁LTE: {}, 解锁NR: {}, 切飞行模式: {}",
                if lock.unlock_lte { "是" } else { "否" },
                if lock.unlock_nr { "是" } else { "否" },
                if lock.toggle_airplane { "是" } else { "否" }
            );
            println!("{}", "=".repeat(60));
        }

        lock
    }

    fn is_night_time(&self) -> Result<bool, Box<dyn Error + Send + Sync>> {
        // 方法1: 使用 UTC+8（中国标准时间）
        let utc_now = Utc::now();

        // 将 UTC 时间转换为东八区时间（中国标准时间）
        // 创建东八区的偏移量（+8小时）
        let china_offset = chrono::FixedOffset::east_opt(8 * 3600)
            .ok_or_else(|| StringError("无效的时区偏移量".to_string()))?;

        let china_time = utc_now.with_timezone(&china_offset);
        let current_time = china_time.time();

        println!("当前UTC时间: {}", utc_now.format("%Y-%m-%d %H:%M:%S"));
        println!("当前中国时间: {}", china_time.format("%Y-%m-%d %H:%M:%S"));

        // 解析时间
        let start_time = NaiveTime::parse_from_str(&self.night_start, "%H:%M")
            .map_err(|e| StringError(format!("解析开始时间失败: {}", e)))?;

        let end_time = NaiveTime::parse_from_str(&self.night_end, "%H:%M")
            .map_err(|e| StringError(format!("解析结束时间失败: {}", e)))?;

        // 处理跨天情况
        if start_time > end_time {
            // 例如 22:00-06:00
            Ok(current_time >= start_time || current_time < end_time)
        } else {
            // 例如 06:00-22:00
            Ok(current_time >= start_time && current_time < end_time)
        }
    }

    fn get_current_mode(&self) -> Result<Option<String>, Box<dyn Error + Send + Sync>> {
        let is_night = self.is_night_time()?;

        if is_night && self.night_enabled {
            Ok(Some("night".to_string()))
        } else if !is_night && self.day_enabled {
            Ok(Some("day".to_string()))
        } else {
            Ok(None)
        }
    }

    fn get_lock_config_for_mode(&self, mode: &str) -> LockConfig {
        if mode == "night" {
            LockConfig {
                lte_type: self.night_lte_type,
                lte_bands: self.night_lte_bands.clone(),
                lte_arfcns: self.night_lte_arfcns.clone(),
                lte_pcis: self.night_lte_pcis.clone(),
                nr_type: self.night_nr_type,
                nr_bands: self.night_nr_bands.clone(),
                nr_arfcns: self.night_nr_arfcns.clone(),
                nr_scs_types: self.night_nr_scs_types.clone(),
                nr_pcis: self.night_nr_pcis.clone(),
            }
        } else if mode == "day" {
            LockConfig {
                lte_type: self.day_lte_type,
                lte_bands: self.day_lte_bands.clone(),
                lte_arfcns: self.day_lte_arfcns.clone(),
                lte_pcis: self.day_lte_pcis.clone(),
                nr_type: self.day_nr_type,
                nr_bands: self.day_nr_bands.clone(),
                nr_arfcns: self.day_nr_arfcns.clone(),
                nr_scs_types: self.day_nr_scs_types.clone(),
                nr_pcis: self.day_nr_pcis.clone(),
            }
        } else {
            LockConfig {
                lte_type: 0,
                lte_bands: "".to_string(),
                lte_arfcns: "".to_string(),
                lte_pcis: "".to_string(),
                nr_type: 0,
                nr_bands: "".to_string(),
                nr_arfcns: "".to_string(),
                nr_scs_types: "".to_string(),
                nr_pcis: "".to_string(),
            }
        }
    }

    async fn check_network_status(&self) -> Result<bool, Box<dyn Error + Send + Sync>> {
        // 查询网络注册状态
        let response = self
            .client
            .send_command("AT+CREG?\r\n".to_string())
            .await
            .map_err(|e| StringError(format!("发送AT命令失败: {}", e)))?;

        // +CREG: 0,1 或 +CREG: 0,5 表示已注册
        if response.contains("+CREG: 0,1") || response.contains("+CREG: 0,5") {
            return Ok(true);
        }

        // 也检查 LTE/5G 注册状态
        let response = self
            .client
            .send_command("AT+CEREG?\r\n".to_string())
            .await
            .map_err(|e| StringError(format!("发送AT命令失败: {}", e)))?;

        if response.contains("+CEREG: 0,1") || response.contains("+CEREG: 0,5") {
            return Ok(true);
        }

        Ok(false)
    }

    async fn set_frequency_lock(&mut self, config: LockConfig, mode: &str) {
        if self.is_switching {
            return;
        }

        self.is_switching = true;
        self.switch_count += 1;

        println!("{}", "=".repeat(60));
        println!(
            "🔄 切换到{}模式锁频设置 (第 {} 次)",
            mode, self.switch_count
        );
        println!("{}", "=".repeat(60));

        let mut operations = Vec::new();

        // 1. 进入飞行模式
        if self.toggle_airplane {
            println!("步骤 1: 进入飞行模式...");
            match self.client.send_command("AT+CFUN=0\r\n".to_string()).await {
                Ok(response) => {
                    if response.contains("OK") {
                        println!("✓ 进入飞行模式");
                        operations.push("切飞行模式".to_string());
                        sleep(Duration::from_secs(2)).await;
                    } else {
                        println!("✗ 进入飞行模式失败");
                    }
                }
                Err(e) => println!("✗ 进入飞行模式失败: {}", e),
            }
        }

        // 2. 设置 LTE 锁频
        let lte_type = config.lte_type;
        if lte_type > 0 {
            let lte_bands = config.lte_bands.trim();
            if !lte_bands.is_empty() {
                let command = self.build_lte_command(
                    lte_type,
                    lte_bands,
                    &config.lte_arfcns,
                    &config.lte_pcis,
                );
                println!("步骤 2: 设置 LTE 锁频 (类型: {})...", lte_type);
                println!("  命令: {}", command.trim());

                match self.client.send_command(command).await {
                    Ok(response) => {
                        if response.contains("OK") {
                            println!("✓ LTE 锁频成功");
                            operations.push(format!("LTE锁频(类型{})", lte_type));
                        } else {
                            println!("✗ LTE 锁频失败: {}", response);
                        }
                    }
                    Err(e) => println!("✗ LTE 锁频失败: {}", e),
                }
                sleep(Duration::from_secs(1)).await;
            }
        } else {
            // 解锁 LTE
            if self.unlock_lte {
                println!("步骤 2: 解锁 LTE...");
                match self
                    .client
                    .send_command("AT^LTEFREQLOCK=0\r\n".to_string())
                    .await
                {
                    Ok(response) => {
                        if response.contains("OK") {
                            println!("✓ LTE 解锁成功");
                            operations.push("LTE解锁".to_string());
                        } else {
                            println!("✗ LTE 解锁失败: {}", response);
                        }
                    }
                    Err(e) => println!("✗ LTE 解锁失败: {}", e),
                }
                sleep(Duration::from_secs(1)).await;
            }
        }

        // 3. 设置 NR 锁频
        let nr_type = config.nr_type;
        if nr_type > 0 {
            let nr_bands = config.nr_bands.trim();
            if !nr_bands.is_empty() {
                let command = self.build_nr_command(
                    nr_type,
                    nr_bands,
                    &config.nr_arfcns,
                    &config.nr_scs_types,
                    &config.nr_pcis,
                );
                println!("步骤 3: 设置 NR 锁频 (类型: {})...", nr_type);
                println!("  命令: {}", command.trim());

                match self.client.send_command(command).await {
                    Ok(response) => {
                        if response.contains("OK") {
                            println!("✓ NR 锁频成功");
                            operations.push(format!("NR锁频(类型{})", nr_type));
                        } else {
                            println!("✗ NR 锁频失败: {}", response);
                        }
                    }
                    Err(e) => println!("✗ NR 锁频失败: {}", e),
                }
                sleep(Duration::from_secs(1)).await;
            }
        } else {
            // 解锁 NR
            if self.unlock_nr {
                println!("步骤 3: 解锁 NR...");
                match self
                    .client
                    .send_command("AT^NRFREQLOCK=0\r\n".to_string())
                    .await
                {
                    Ok(response) => {
                        if response.contains("OK") {
                            println!("✓ NR 解锁成功");
                            operations.push("NR解锁".to_string());
                        } else {
                            println!("✗ NR 解锁失败: {}", response);
                        }
                    }
                    Err(e) => println!("✗ NR 解锁失败: {}", e),
                }
                sleep(Duration::from_secs(1)).await;
            }
        }

        // 4. 退出飞行模式使配置生效
        if self.toggle_airplane {
            println!("步骤 4: 退出飞行模式使配置生效...");
            match self.client.send_command("AT+CFUN=1\r\n".to_string()).await {
                Ok(response) => {
                    if response.contains("OK") {
                        println!("✓ 退出飞行模式");
                    } else {
                        println!("✗ 退出飞行模式失败");
                    }
                }
                Err(e) => println!("✗ 退出飞行模式失败: {}", e),
            }
            sleep(Duration::from_secs(3)).await;
        }

        // 发送通知
        let ops_text = if operations.is_empty() {
            "未执行任何操作".to_string()
        } else {
            operations.join("、")
        };

        let lte_info = if lte_type > 0 {
            format!("LTE类型{}", lte_type)
        } else {
            "LTE解锁".to_string()
        };

        let nr_info = if nr_type > 0 {
            format!("NR类型{}", nr_type)
        } else {
            "NR解锁".to_string()
        };

        let now = Local::now();
        let timestamp = now.format("%Y-%m-%d %H:%M:%S").to_string();

        println!("{}", "=".repeat(60));
        println!("✓ 定时锁频切换完成");
        println!("  时间: {}", timestamp);
        println!("  模式: {}模式", mode);
        println!("  LTE: {}", lte_info);
        println!("  NR: {}", nr_info);
        println!("  执行操作: {}", ops_text);
        println!("  切换次数: 第 {} 次", self.switch_count);
        println!("{}", "=".repeat(60));

        self.is_switching = false;
    }

    fn build_lte_command(&self, lock_type: u8, bands: &str, arfcns: &str, pcis: &str) -> String {
        if lock_type == 0 {
            return "AT^LTEFREQLOCK=0\r\n".to_string();
        }

        let band_list: Vec<&str> = bands
            .split(',')
            .map(|b| b.trim())
            .filter(|b| !b.is_empty())
            .collect();

        if lock_type == 3 {
            // 频段锁定
            if band_list.is_empty() {
                return "AT^LTEFREQLOCK=0\r\n".to_string();
            }
            return format!(
                "AT^LTEFREQLOCK=3,0,{},\"{}\"\r\n",
                band_list.len(),
                band_list.join(",")
            );
        } else if lock_type == 1 {
            // 频点锁定
            let arfcn_list: Vec<&str> = arfcns
                .split(',')
                .map(|a| a.trim())
                .filter(|a| !a.is_empty())
                .collect();

            if band_list.is_empty() || arfcn_list.is_empty() || band_list.len() != arfcn_list.len()
            {
                println!("LTE 频点锁定：频段和频点数量不匹配，解锁");
                return "AT^LTEFREQLOCK=0\r\n".to_string();
            }

            return format!(
                "AT^LTEFREQLOCK=1,0,{},\"{}\",\"{}\"\r\n",
                band_list.len(),
                band_list.join(","),
                arfcn_list.join(",")
            );
        } else if lock_type == 2 {
            // 小区锁定
            let arfcn_list: Vec<&str> = arfcns
                .split(',')
                .map(|a| a.trim())
                .filter(|a| !a.is_empty())
                .collect();
            let pci_list: Vec<&str> = pcis
                .split(',')
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .collect();

            if band_list.is_empty()
                || arfcn_list.is_empty()
                || pci_list.is_empty()
                || band_list.len() != arfcn_list.len()
                || arfcn_list.len() != pci_list.len()
            {
                println!("LTE 小区锁定：频段、频点、PCI 数量不匹配，解锁");
                return "AT^LTEFREQLOCK=0\r\n".to_string();
            }

            return format!(
                "AT^LTEFREQLOCK=2,0,{},\"{}\",\"{}\",\"{}\"\r\n",
                band_list.len(),
                band_list.join(","),
                arfcn_list.join(","),
                pci_list.join(",")
            );
        } else {
            return "AT^LTEFREQLOCK=0\r\n".to_string();
        }
    }

    fn build_nr_command(
        &self,
        lock_type: u8,
        bands: &str,
        arfcns: &str,
        scs_types: &str,
        pcis: &str,
    ) -> String {
        if lock_type == 0 {
            return "AT^NRFREQLOCK=0\r\n".to_string();
        }

        let band_list: Vec<&str> = bands
            .split(',')
            .map(|b| b.trim())
            .filter(|b| !b.is_empty())
            .collect();

        if lock_type == 3 {
            // 频段锁定
            if band_list.is_empty() {
                return "AT^NRFREQLOCK=0\r\n".to_string();
            }
            return format!(
                "AT^NRFREQLOCK=3,0,{},\"{}\"\r\n",
                band_list.len(),
                band_list.join(",")
            );
        } else if lock_type == 1 {
            // 频点锁定
            let arfcn_list: Vec<&str> = arfcns
                .split(',')
                .map(|a| a.trim())
                .filter(|a| !a.is_empty())
                .collect();
            let scs_list: Vec<String> = scs_types
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            if band_list.is_empty() || arfcn_list.is_empty() || band_list.len() != arfcn_list.len()
            {
                println!("NR 频点锁定：频段和频点数量不匹配，解锁");
                return "AT^NRFREQLOCK=0\r\n".to_string();
            }

            let final_scs_list = if scs_list.is_empty() || scs_list.len() != band_list.len() {
                self.auto_detect_scs_types(&band_list, &arfcn_list)
            } else {
                scs_list
            };

            if final_scs_list.len() != band_list.len() {
                println!("NR 频点锁定：SCS 类型数量不匹配，解锁");
                return "AT^NRFREQLOCK=0\r\n".to_string();
            }

            return format!(
                "AT^NRFREQLOCK=1,0,{},\"{}\",\"{}\",\"{}\"\r\n",
                band_list.len(),
                band_list.join(","),
                arfcn_list.join(","),
                final_scs_list.join(",")
            );
        } else if lock_type == 2 {
            // 小区锁定
            let arfcn_list: Vec<&str> = arfcns
                .split(',')
                .map(|a| a.trim())
                .filter(|a| !a.is_empty())
                .collect();
            let scs_list: Vec<String> = scs_types
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let pci_list: Vec<&str> = pcis
                .split(',')
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .collect();

            if band_list.is_empty()
                || arfcn_list.is_empty()
                || pci_list.is_empty()
                || band_list.len() != arfcn_list.len()
                || arfcn_list.len() != pci_list.len()
            {
                println!("NR 小区锁定：频段、频点、PCI 数量不匹配，解锁");
                return "AT^NRFREQLOCK=0\r\n".to_string();
            }

            let final_scs_list = if scs_list.is_empty() || scs_list.len() != band_list.len() {
                self.auto_detect_scs_types(&band_list, &arfcn_list)
            } else {
                scs_list
            };

            if final_scs_list.len() != band_list.len() {
                println!("NR 小区锁定：SCS 类型数量不匹配，解锁");
                return "AT^NRFREQLOCK=0\r\n".to_string();
            }

            return format!(
                "AT^NRFREQLOCK=2,0,{},\"{}\",\"{}\",\"{}\",\"{}\"\r\n",
                band_list.len(),
                band_list.join(","),
                arfcn_list.join(","),
                final_scs_list.join(","),
                pci_list.join(",")
            );
        } else {
            return "AT^NRFREQLOCK=0\r\n".to_string();
        }
    }

    fn auto_detect_scs_types(&self, bands: &[&str], arfcns: &[&str]) -> Vec<String> {
        let mut scs_list = Vec::new();

        for i in 0..bands.len().min(arfcns.len()) {
            let band = bands[i];
            let _arfcn = arfcns[i];

            // 根据频段自动识别 SCS 类型
            let scs = if let Ok(band_num) = band.parse::<i32>() {
                match band_num {
                    78 | 79 | 258 | 260 => "1", // n78, n79, n258, n260 通常使用 30kHz SCS
                    41 | 77 => "1",             // n41, n77 通常使用 30kHz SCS
                    28 | 71 => "0",             // n28, n71 通常使用 15kHz SCS
                    _ => "1",                   // 默认使用 30kHz SCS
                }
            } else {
                "1" // 默认使用 30kHz SCS
            };

            scs_list.push(scs.to_string());
        }

        scs_list
    }

    async fn monitor_loop(mut self) {
        if !self.enabled {
            println!("定时锁频功能已禁用");
            return;
        }

        println!("启动定时锁频监控...");

        loop {
            // 使用局部变量来避免跨越 await 的借用
            let current_mode_result = self.get_current_mode();

            match current_mode_result {
                Ok(target_mode) => {
                    if let Some(ref target_mode_str) = target_mode {
                        if Some(target_mode_str) != self.current_mode.as_ref() {
                            // 模式发生变化，执行切换
                            let config = self.get_lock_config_for_mode(target_mode_str);
                            println!(
                                "检测到模式切换: {:?} -> {}",
                                self.current_mode, target_mode_str
                            );
                            self.set_frequency_lock(config, target_mode_str).await;
                            self.current_mode = target_mode.clone();
                        }
                    } else if target_mode.is_none() && self.current_mode.is_some() {
                        // 当前时段不需要锁频，如果之前有锁频则解锁
                        println!("当前时段不需要锁频，解锁所有频段");
                        let unlock_config = LockConfig {
                            lte_type: 0,
                            lte_bands: "".to_string(),
                            lte_arfcns: "".to_string(),
                            lte_pcis: "".to_string(),
                            nr_type: 0,
                            nr_bands: "".to_string(),
                            nr_arfcns: "".to_string(),
                            nr_scs_types: "".to_string(),
                            nr_pcis: "".to_string(),
                        };
                        self.set_frequency_lock(unlock_config, "解锁").await;
                        self.current_mode = None;
                    }

                    // 检查网络状态（用于超时检测）
                    // match self.check_network_status().await {
                    //     Ok(has_service) => {
                    //         if has_service {
                    //             // 有服务，更新最后服务时间
                    //             self.last_service_time = SystemTime::now()
                    //                 .duration_since(UNIX_EPOCH)
                    //                 .unwrap()
                    //                 .as_secs();
                    //         } else {
                    //             // 无服务，检查是否超时
                    //             let current_time = SystemTime::now()
                    //                 .duration_since(UNIX_EPOCH)
                    //                 .unwrap()
                    //                 .as_secs();
                    //             let no_service_duration = current_time - self.last_service_time;

                    //             if no_service_duration >= self.timeout {
                    //                 // 超时，执行恢复（解锁所有频段）
                    //                 println!(
                    //                     "检测到网络长时间无服务 ({}秒)，执行恢复",
                    //                     no_service_duration
                    //                 );
                    //                 let unlock_config = LockConfig {
                    //                     lte_type: 0,
                    //                     lte_bands: "".to_string(),
                    //                     lte_arfcns: "".to_string(),
                    //                     lte_pcis: "".to_string(),
                    //                     nr_type: 0,
                    //                     nr_bands: "".to_string(),
                    //                     nr_arfcns: "".to_string(),
                    //                     nr_scs_types: "".to_string(),
                    //                     nr_pcis: "".to_string(),
                    //                 };
                    //                 self.set_frequency_lock(unlock_config, "恢复").await;
                    //                 // 重置计时器
                    //                 self.last_service_time = current_time;
                    //             } else {
                    //                 println!("无服务状态持续 {} 秒", no_service_duration);
                    //             }
                    //         }
                    //     }
                    //     Err(e) => println!("检查网络状态失败: {}", e),
                    // }
                }
                Err(e) => println!("获取当前模式失败: {}", e),
            }

            // 等待下次检查
            sleep(Duration::from_secs(self.check_interval)).await;
        }
    }
}
*/

struct LockConfig {
    lte_type: u8,
    lte_bands: String,
    lte_arfcns: String,
    lte_pcis: String,
    nr_type: u8,
    nr_bands: String,
    nr_arfcns: String,
    nr_scs_types: String,
    nr_pcis: String,
}

/// 创建双栈监听器，同时支持IPv4和IPv6
async fn create_dual_stack_listener(host: &str, port: u16) -> Result<TcpListener, Box<dyn Error>> {
    use std::net::SocketAddr;
    use tokio::net::TcpSocket;

    // 解析IPv6地址
    let ipv6_addr = if host == "::" {
        Ipv6Addr::UNSPECIFIED
    } else {
        Ipv6Addr::from_str(host).map_err(|e| format!("无效的IPv6地址: {}", e))?
    };

    let socket_addr = SocketAddr::new(IpAddr::V6(ipv6_addr), port);

    // 创建IPv6套接字
    let socket = TcpSocket::new_v6()?;

    // 设置套接字选项：允许IPv4映射（在Linux上默认启用）
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = socket.as_raw_fd();

        // 设置IPV6_V6ONLY为0，允许IPv4映射
        let enable: libc::c_int = 0;
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::IPPROTO_IPV6,
                libc::IPV6_V6ONLY,
                &enable as *const _ as *const libc::c_void,
                std::mem::size_of_val(&enable) as libc::socklen_t,
            )
        };
        if ret != 0 {
            return Err(format!("设置IPV6_V6ONLY失败: {}", std::io::Error::last_os_error()).into());
        }
    }

    // 绑定地址
    socket.bind(socket_addr)?;

    // 开始监听
    let listener = socket.listen(1024)?;

    Ok(listener)
}

/// 备用方案：使用std::net创建监听器，然后转换为tokio的TcpListener
#[allow(dead_code)]
async fn create_dual_stack_listener_alt(
    host: &str,
    port: u16,
) -> Result<TcpListener, Box<dyn Error>> {
    use std::net::TcpListener as StdTcpListener;

    // 解析IPv6地址
    let ipv6_addr = if host == "::" {
        Ipv6Addr::UNSPECIFIED
    } else {
        Ipv6Addr::from_str(host).map_err(|e| format!("无效的IPv6地址: {}", e))?
    };

    let socket_addr = std::net::SocketAddr::new(IpAddr::V6(ipv6_addr), port);

    // 创建std TcpListener
    let std_listener = StdTcpListener::bind(socket_addr)?;

    // 设置为非阻塞（需要tokio使用）
    std_listener.set_nonblocking(true)?;

    // 转换为tokio的TcpListener
    let listener = TcpListener::from_std(std_listener)?;

    Ok(listener)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // 从UCI加载配置
    let config = match load_config_from_uci() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("从UCI加载配置失败: {}, 使用默认配置", e);
            serde_json::from_str(DEFAULT_CONFIG_JSON)?
        }
    };

    let config = Arc::new(config);

    // 打印配置信息
    println!("{}", "=".repeat(60));
    println!("当前运行配置:");
    println!("{}", "=".repeat(60));
    println!("连接类型: {}", config.at_config.conn_type);

    if config.at_config.conn_type == "NETWORK" {
        println!(
            "  网络地址: {}:{}",
            config.at_config.network.host, config.at_config.network.port
        );
        println!("  网络超时: {}秒", config.at_config.network.timeout);
    } else {
        println!("  串口设备: {}", config.at_config.serial.port);
        println!("  波特率: {}", config.at_config.serial.baudrate);
        println!("  串口超时: {}秒", config.at_config.serial.timeout);
        println!("  串口方法: {}", config.at_config.serial.method);
        println!("  串口功能: {}", config.at_config.serial.feature);
    }

    println!("\nWebSocket 配置:");
    println!("  监听端口: {}", config.websocket_config.ipv4.port);
    println!("  IPv4 绑定: {}", config.websocket_config.ipv4.host);
    println!("  IPv6 绑定: {}", config.websocket_config.ipv6.host);
    println!(
        "  认证密钥: {}",
        if config.websocket_config.auth_key.is_empty() {
            "无"
        } else {
            "已设置"
        }
    );

    println!("\n通知配置:");
    println!(
        "  企业微信: {}",
        if config.notification_config.wechat_webhook.is_empty() {
            "未启用"
        } else {
            "已启用"
        }
    );
    println!(
        "  日志文件: {}",
        if config.notification_config.log_file.is_empty() {
            "未启用"
        } else {
            &config.notification_config.log_file
        }
    );

    println!("  通知类型:");
    println!(
        "    - 短信通知: {}",
        if config.notification_config.notification_types.sms {
            "✓ 启用"
        } else {
            "✗ 禁用"
        }
    );
    println!(
        "    - 来电通知: {}",
        if config.notification_config.notification_types.call {
            "✓ 启用"
        } else {
            "✗ 禁用"
        }
    );
    println!(
        "    - 存储满通知: {}",
        if config.notification_config.notification_types.memory_full {
            "✓ 启用"
        } else {
            "✗ 禁用"
        }
    );
    println!(
        "    - 信号通知: {}",
        if config.notification_config.notification_types.signal {
            "✓ 启用"
        } else {
            "✗ 禁用"
        }
    );

    println!("\n自动重启飞行模式配置:");
    println!(
        "  启用: {}",
        if config.auto_airplane.enabled {
            "是"
        } else {
            "否"
        }
    );
    println!(
        "重启执行时间：{} ",
        if config.auto_airplane.action_time.is_empty() {
            "未设置".to_string()
        } else {
            config.auto_airplane.action_time.clone()
        }
    );

    /* 暂不启用定时锁频监控
    println!("\n定时锁频配置:");
    println!(
        "  启用: {}",
        if config.schedule_config.enabled {
            "是"
        } else {
            "否"
        }
    );
    if config.schedule_config.enabled {
        println!("  检测间隔: {}秒", config.schedule_config.check_interval);
        println!("  超时时间: {}秒", config.schedule_config.timeout);
        println!(
            "  夜间模式: {} ({}-{})",
            if config.schedule_config.night_enabled {
                "启用"
            } else {
                "禁用"
            },
            config.schedule_config.night_start,
            config.schedule_config.night_end
        );
        println!(
            "  日间模式: {}",
            if config.schedule_config.day_enabled {
                "启用"
            } else {
                "禁用"
            }
        );
    }
    */
   
    println!("{}", "=".repeat(60));

    // 创建AT客户端
    let at_client = Arc::new(ATClient::new(config.clone())?);

    /*
    // 创建定时锁频监控器
    let schedule_lock = ScheduleFrequencyLock::new(at_client.clone(), config.clone());

    // 启动定时锁频监控任务（如果启用）
    if schedule_lock.enabled {
        tokio::spawn(async move {
            schedule_lock.monitor_loop().await;
        });
    }
    */

    // 创建自动重启飞行模式监控
    let auto_flight_mode = AutoAirPlaneMode::new(at_client.clone(), config.clone());

    if auto_flight_mode.enabled {
        tokio::spawn(async move {
            auto_flight_mode.monitor_loop().await;
        });
    }

    // 心跳任务
    let c_heartbeat = at_client.clone();
    tokio::spawn(async move {
        let mut heartbeat_timer = interval(Duration::from_secs(30));
        loop {
            heartbeat_timer.tick().await;
            {
                let mut conn = c_heartbeat.conn.lock().await;
                if conn.is_connected() {
                    let _ = conn.send(b"ping\r\n").await;
                }
            }
        }
    });

    // URC 捕获任务
    let c_monitor = at_client.clone();
    tokio::spawn(async move {
        loop {
            let mut has_data = false;
            {
                let mut conn = c_monitor.conn.lock().await;
                if !conn.is_connected() {
                    if let Ok(_) = conn.connect().await {
                        println!("Module Connected.");
                        drop(conn);
                        let c_init = c_monitor.clone();
                        tokio::spawn(async move { c_init.init_module().await });
                    }
                } else {
                    if let Ok(data) = conn.receive().await {
                        if !data.is_empty() {
                            has_data = true;
                            let text = String::from_utf8_lossy(&data).to_string();
                            for line in text.lines() {
                                let l = line.trim();
                                if !l.is_empty() && !l.to_lowercase().contains("ping") {
                                    if l.contains("^") || l.contains("+") {
                                        println!("[URC DETECTED] <== {:?}", line);
                                        let _ = c_monitor.urc_tx.send(line.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if !has_data {
                sleep(Duration::from_millis(20)).await;
            }
        }
    });

    let ws_handler = |stream: TcpStream,
                      addr: std::net::SocketAddr,
                      client: Arc<ATClient>,
                      auth_key: String| async move {
        let ws_stream = accept_async(stream).await.ok()?;
        let (mut ws_tx, mut ws_rx) = ws_stream.split();
        let mut urc_rx = client.urc_tx.subscribe();

        // 打印连接信息
        println!("[WebSocket] 新连接: {}", addr);

        // 如果配置了认证密钥，需要先进行认证
        if !auth_key.is_empty() {
            // 等待客户端发送认证信息，设置10秒超时
            let auth_result = timeout(Duration::from_secs(10), async {
                if let Some(Ok(Message::Text(auth_msg))) = ws_rx.next().await {
                    let auth_data: Result<serde_json::Value, _> = serde_json::from_str(&auth_msg);
                    if let Ok(auth_data) = auth_data {
                        if let Some(client_key) = auth_data.get("auth_key") {
                            if client_key.as_str() == Some(&auth_key) {
                                return true;
                            }
                        }
                    }
                }
                false
            })
            .await
            .unwrap_or(false);

            if !auth_result {
                // 认证失败，关闭连接
                println!("[WebSocket] 认证失败: {}", addr);
                let _ = ws_tx
                    .send(Message::Text(
                        serde_json::json!({
                            "error": "Authentication failed",
                            "message": "密钥验证失败"
                        })
                        .to_string(),
                    ))
                    .await;
                return None;
            }

            // 认证成功
            let _ = ws_tx
                .send(Message::Text(
                    serde_json::json!({
                        "success": true,
                        "message": "认证成功"
                    })
                    .to_string(),
                ))
                .await;
            println!("[WebSocket] 认证成功: {}", addr);
        }

        loop {
            tokio::select! {
                urc_res = urc_rx.recv() => {
                    if let Ok(msg) = urc_res {
                        let payload = serde_json::json!({ "type": "raw_data", "data": msg });
                        if let Ok(json_str) = serde_json::to_string(&payload) {
                            if let Err(_) = ws_tx.send(Message::Text(json_str)).await { break; }
                        }
                    }
                }
                msg = ws_rx.next() => {
                    if let Some(Ok(Message::Text(cmd))) = msg {
                        let res = match client.send_command(cmd).await {
                            Ok(r) => serde_json::json!({ "success": true, "data": r, "error": null }),
                            Err(e) => serde_json::json!({ "success": false, "data": null, "error": e.to_string() }),
                        };
                        let _ = ws_tx.send(Message::Text(serde_json::to_string(&res).unwrap())).await;
                    } else { break; }
                }
            }
        }
        println!("[WebSocket] 连接断开: {}", addr);
        Some(())
    };

    // 获取WebSocket配置
    let ws_v6_host = config.websocket_config.ipv6.host.clone();
    let ws_v6_port = config.websocket_config.ipv6.port;
    let auth_key = config.websocket_config.auth_key.clone();

    // 尝试绑定IPv6地址（双栈，支持IPv4映射）
    println!("尝试绑定IPv6双栈监听器...");

    let ws_listener = match create_dual_stack_listener(&ws_v6_host, ws_v6_port).await {
        Ok(listener) => {
            println!(
                "✓ 成功绑定IPv6双栈监听器: [{}]:{}",
                if ws_v6_host == "::" {
                    "::"
                } else {
                    &ws_v6_host
                },
                ws_v6_port
            );
            listener
        }
        Err(e) => {
            println!("⚠ 无法绑定IPv6双栈监听器: {}, 尝试绑定IPv4...", e);
            // 回退到只绑定IPv4
            let ws_v4_addr = format!(
                "{}:{}",
                config.websocket_config.ipv4.host, config.websocket_config.ipv4.port
            );
            match TcpListener::bind(&ws_v4_addr).await {
                Ok(listener) => {
                    println!("✓ 成功绑定IPv4监听器: {}", ws_v4_addr);
                    listener
                }
                Err(e) => {
                    eprintln!("❌ 无法绑定IPv4监听器 {}: {}", ws_v4_addr, e);
                    return Err(e.into());
                }
            }
        }
    };

    println!("--------------------------------------");
    println!("AT WebSocket 服务器启动成功！");
    println!("监听端口: {}", ws_v6_port);
    println!("支持协议: IPv4 和 IPv6 (双栈)");
    if !auth_key.is_empty() {
        println!("认证模式: 已启用 (密钥长度: {})", auth_key.len());
    } else {
        println!("认证模式: 未启用 (允许无密钥访问)");
    }
    if config.schedule_config.enabled {
        println!(
            "定时锁频: 已启用 (检测间隔: {}秒)",
            config.schedule_config.check_interval
        );
    }
    println!("--------------------------------------");

    let client = at_client.clone();

    // 启动WebSocket服务器
    println!("WebSocket 服务器运行中...");
    loop {
        match ws_listener.accept().await {
            Ok((stream, addr)) => {
                tokio::spawn(ws_handler(stream, addr, client.clone(), auth_key.clone()));
            }
            Err(e) => {
                eprintln!("接受连接失败: {}", e);
                break;
            }
        }
    }

    Ok(())
}

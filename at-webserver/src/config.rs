use crate::models::ConnectionType;
use std::process::Command;
use log::{info, error};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Config {
    pub at_config: AtConfig,
    pub notification_config: NotificationConfig,
    pub websocket_config: WebSocketConfig,
    pub schedule_config: ScheduleConfig,
    pub advanced_network_config: AdvancedNetworkConfig,
}

#[derive(Debug, Clone)]
pub struct AtConfig {
    pub connection_type: ConnectionType,
    pub network: NetworkConfig,
    pub serial: SerialConfig,
}

#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub host: String,
    pub port: u16,
    pub timeout: u64,
}

#[derive(Debug, Clone)]
pub struct SerialConfig {
    pub port: String,
    pub baudrate: u32,
    pub timeout: u64,
}

#[derive(Debug, Clone)]
pub struct NotificationConfig {
    pub wechat_webhook: Option<String>,
    pub log_file: Option<String>,
    pub notify_sms: bool,
    pub notify_call: bool,
    pub notify_memory_full: bool,
    pub notify_signal: bool,
}

#[derive(Debug, Clone)]
pub struct WebSocketConfig {
    pub ipv4: IpConfig,
    pub ipv6: IpConfig,
    pub auth_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IpConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone)]
pub struct ScheduleConfig {
    pub enabled: bool,
    pub check_interval: u64,
    pub timeout: u64,
    pub unlock_lte: bool,
    pub unlock_nr: bool,
    pub toggle_airplane: bool,
    
    // Night Mode
    pub night_enabled: bool,
    pub night_start: String,
    pub night_end: String,
    pub night_lte_type: u8,
    pub night_lte_bands: String,
    pub night_lte_arfcns: String,
    pub night_lte_pcis: String,
    pub night_nr_type: u8,
    pub night_nr_bands: String,
    pub night_nr_arfcns: String,
    pub night_nr_scs_types: String,
    pub night_nr_pcis: String,
    
    // Day Mode
    pub day_enabled: bool,
    pub day_lte_type: u8,
    pub day_lte_bands: String,
    pub day_lte_arfcns: String,
    pub day_lte_pcis: String,
    pub day_nr_type: u8,
    pub day_nr_bands: String,
    pub day_nr_arfcns: String,
    pub day_nr_scs_types: String,
    pub day_nr_pcis: String,
}

#[derive(Debug, Clone)]
pub struct AdvancedNetworkConfig {
    pub pdp_type: String,
    pub ra_master: bool,
    pub extend_prefix: bool,
    pub do_not_add_dns: bool,
    pub dns_list: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            at_config: AtConfig {
                connection_type: ConnectionType::Network,
                network: NetworkConfig {
                    host: "192.168.8.1".to_string(),
                    port: 20249,
                    timeout: 10,
                },
                serial: SerialConfig {
                    port: "/dev/ttyUSB0".to_string(),
                    baudrate: 115200,
                    timeout: 10,
                },
            },
            notification_config: NotificationConfig {
                wechat_webhook: None,
                log_file: None,
                notify_sms: true,
                notify_call: true,
                notify_memory_full: true,
                notify_signal: true,
            },
            websocket_config: WebSocketConfig {
                ipv4: IpConfig {
                    host: "0.0.0.0".to_string(),
                    port: 8765,
                },
                ipv6: IpConfig {
                    host: "::".to_string(),
                    port: 8765,
                },
                auth_key: None,
            },
            schedule_config: ScheduleConfig {
                enabled: false,
                check_interval: 60,
                timeout: 180,
                unlock_lte: true,
                unlock_nr: true,
                toggle_airplane: true,
                night_enabled: true,
                night_start: "22:00".to_string(),
                night_end: "06:00".to_string(),
                night_lte_type: 3,
                night_lte_bands: "".to_string(),
                night_lte_arfcns: "".to_string(),
                night_lte_pcis: "".to_string(),
                night_nr_type: 3,
                night_nr_bands: "".to_string(),
                night_nr_arfcns: "".to_string(),
                night_nr_scs_types: "".to_string(),
                night_nr_pcis: "".to_string(),
                day_enabled: true,
                day_lte_type: 3,
                day_lte_bands: "".to_string(),
                day_lte_arfcns: "".to_string(),
                day_lte_pcis: "".to_string(),
                day_nr_type: 3,
                day_nr_bands: "".to_string(),
                day_nr_arfcns: "".to_string(),
                day_nr_scs_types: "".to_string(),
                day_nr_pcis: "".to_string(),
            },
            advanced_network_config: AdvancedNetworkConfig {
                pdp_type: "ipv4v6".to_string(),
                ra_master: false,
                extend_prefix: true,
                do_not_add_dns: false,
                dns_list: vec!["223.5.5.5".to_string(), "119.29.29.29".to_string()],
            },
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let mut config = Config::default();
        let mut uci_data = HashMap::new();

        info!("Loading configuration from UCI...");

        // Run `uci show at-webserver`
        match Command::new("uci").args(&["show", "at-webserver"]).output() {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    if let Some((key, value)) = line.split_once('=') {
                        if key.starts_with("at-webserver.config.") {
                            let short_key = key.trim_start_matches("at-webserver.config.");
                            let clean_value = value.trim().trim_matches('\'').trim_matches('"').to_string();
                            uci_data.insert(short_key.to_string(), clean_value);
                        }
                    }
                }
            }
            Ok(_) => {
                error!("UCI command returned non-zero status. Using default config.");
            }
            Err(e) => {
                error!("Failed to execute UCI command: {}. Using default config.", e);
            }
        }

        // Helper to get string value
        let get_str = |key: &str, default: &str| -> String {
            uci_data.get(key).cloned().unwrap_or_else(|| default.to_string())
        };

        // Helper to get bool value
        let get_bool = |key: &str, default: bool| -> bool {
            match uci_data.get(key).map(|s| s.as_str()) {
                Some("1") | Some("true") | Some("on") => true,
                Some("0") | Some("false") | Some("off") => false,
                _ => default,
            }
        };

        // Helper to get int value
        let get_int = |key: &str, default: u64| -> u64 {
            uci_data.get(key).and_then(|s| s.parse().ok()).unwrap_or(default)
        };
        
        let get_u32 = |key: &str, default: u32| -> u32 {
            uci_data.get(key).and_then(|s| s.parse().ok()).unwrap_or(default)
        };
        
        let get_u16 = |key: &str, default: u16| -> u16 {
            uci_data.get(key).and_then(|s| s.parse().ok()).unwrap_or(default)
        };
        
        let get_u8 = |key: &str, default: u8| -> u8 {
            uci_data.get(key).and_then(|s| s.parse().ok()).unwrap_or(default)
        };

        // AT Config
        let conn_type_str = get_str("connection_type", "NETWORK");
        if conn_type_str == "SERIAL" {
            config.at_config.connection_type = ConnectionType::Serial;
        } else {
            config.at_config.connection_type = ConnectionType::Network;
        }

        config.at_config.network.host = get_str("network_host", "192.168.8.1");
        config.at_config.network.port = get_u16("network_port", 20249);
        config.at_config.network.timeout = get_int("network_timeout", 10);

        let mut serial_port = get_str("serial_port", "/dev/ttyUSB0");
        if serial_port == "custom" {
            serial_port = get_str("serial_port_custom", "/dev/ttyUSB0");
        }
        config.at_config.serial.port = serial_port;
        config.at_config.serial.baudrate = get_u32("serial_baudrate", 115200);
        config.at_config.serial.timeout = get_int("serial_timeout", 10);

        // Notification Config
        let wechat = get_str("wechat_webhook", "");
        config.notification_config.wechat_webhook = if wechat.is_empty() { None } else { Some(wechat) };

        let log_file = get_str("log_file", "");
        config.notification_config.log_file = if log_file.is_empty() { None } else { Some(log_file) };

        config.notification_config.notify_sms = get_bool("notify_sms", true);
        config.notification_config.notify_call = get_bool("notify_call", true);
        config.notification_config.notify_memory_full = get_bool("notify_memory_full", true);
        config.notification_config.notify_signal = get_bool("notify_signal", true);

        // WebSocket Config
        let ws_port = get_u16("websocket_port", 8765);
        config.websocket_config.ipv4.port = ws_port;
        config.websocket_config.ipv6.port = ws_port;
        
        let auth_key = get_str("websocket_auth_key", "");
        config.websocket_config.auth_key = if auth_key.is_empty() { None } else { Some(auth_key) };

        // Schedule Config
        config.schedule_config.enabled = get_bool("schedule_enabled", false);
        config.schedule_config.check_interval = get_int("schedule_check_interval", 60);
        config.schedule_config.timeout = get_int("schedule_timeout", 180);
        config.schedule_config.unlock_lte = get_bool("schedule_unlock_lte", true);
        config.schedule_config.unlock_nr = get_bool("schedule_unlock_nr", true);
        config.schedule_config.toggle_airplane = get_bool("schedule_toggle_airplane", true);

        config.schedule_config.night_enabled = get_bool("schedule_night_enabled", true);
        config.schedule_config.night_start = get_str("schedule_night_start", "22:00");
        config.schedule_config.night_end = get_str("schedule_night_end", "06:00");
        config.schedule_config.night_lte_type = get_u8("schedule_night_lte_type", 3);
        config.schedule_config.night_lte_bands = get_str("schedule_night_lte_bands", "");
        config.schedule_config.night_lte_arfcns = get_str("schedule_night_lte_arfcns", "");
        config.schedule_config.night_lte_pcis = get_str("schedule_night_lte_pcis", "");
        config.schedule_config.night_nr_type = get_u8("schedule_night_nr_type", 3);
        config.schedule_config.night_nr_bands = get_str("schedule_night_nr_bands", "");
        config.schedule_config.night_nr_arfcns = get_str("schedule_night_nr_arfcns", "");
        config.schedule_config.night_nr_scs_types = get_str("schedule_night_nr_scs_types", "");
        config.schedule_config.night_nr_pcis = get_str("schedule_night_nr_pcis", "");

        config.schedule_config.day_enabled = get_bool("schedule_day_enabled", true);
        config.schedule_config.day_lte_type = get_u8("schedule_day_lte_type", 3);
        config.schedule_config.day_lte_bands = get_str("schedule_day_lte_bands", "");
        config.schedule_config.day_lte_arfcns = get_str("schedule_day_lte_arfcns", "");
        config.schedule_config.day_lte_pcis = get_str("schedule_day_lte_pcis", "");
        config.schedule_config.day_nr_type = get_u8("schedule_day_nr_type", 3);
        config.schedule_config.day_nr_bands = get_str("schedule_day_nr_bands", "");
        config.schedule_config.day_nr_arfcns = get_str("schedule_day_nr_arfcns", "");
        config.schedule_config.day_nr_scs_types = get_str("schedule_day_nr_scs_types", "");
        config.schedule_config.day_nr_pcis = get_str("schedule_day_nr_pcis", "");

        // Advanced Network Config
        config.advanced_network_config.pdp_type = get_str("pdp_type", "ipv4v6");
        config.advanced_network_config.ra_master = get_bool("ra_master", false);
        config.advanced_network_config.extend_prefix = get_bool("extend_prefix", true);
        config.advanced_network_config.do_not_add_dns = get_bool("do_not_add_dns", false);
        
        // Fetch dns_list separately as it's a list
        if let Ok(output) = Command::new("uci").args(&["get", "at-webserver.config.dns_list"]).output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let dns_list: Vec<String> = stdout.split_whitespace().map(|s| s.to_string()).collect();
                if !dns_list.is_empty() {
                    config.advanced_network_config.dns_list = dns_list;
                }
            }
        }

        // Env var overrides (for local debugging)
        if let Ok(val) = std::env::var("AT_CONNECTION_TYPE") {
            match val.as_str() {
                "SERIAL" => config.at_config.connection_type = ConnectionType::Serial,
                "NETWORK" => config.at_config.connection_type = ConnectionType::Network,
                _ => {}
            }
        }
        if let Ok(val) = std::env::var("AT_NETWORK_HOST") { config.at_config.network.host = val; }
        if let Ok(val) = std::env::var("AT_NETWORK_PORT") { 
            if let Ok(p) = val.parse() { config.at_config.network.port = p; }
        }
        if let Ok(val) = std::env::var("AT_SERIAL_PORT") { config.at_config.serial.port = val; }
        if let Ok(val) = std::env::var("AT_SERIAL_BAUDRATE") { 
             if let Ok(b) = val.parse() { config.at_config.serial.baudrate = b; }
        }
        if let Ok(val) = std::env::var("AT_LOG_FILE") { config.notification_config.log_file = Some(val); }

        info!("Loaded configuration: {:?}", config);
        config
    }
}

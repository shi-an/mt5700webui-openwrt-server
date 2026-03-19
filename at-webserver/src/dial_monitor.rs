use crate::client::ATClient;
use crate::config::Config;
use crate::models::get_ndis_disconnect_tx;
use crate::network;
use log::{info, warn, error, debug};
use std::time::Duration;
use tokio::time::{sleep, interval};
use tokio::process::Command;
use anyhow::Result;

use tokio::fs;

/// IP 连接状态，参考 QModem modem_dial.sh 的 connection_status 四状态设计
#[derive(Debug, Clone, PartialEq)]
enum IpStatus {
    /// AT 命令响应异常（非预期内容）
    Unexpected,
    /// 有响应但无有效 IP
    NoIp,
    /// 仅 IPv4
    Ipv4Only(String),
    /// 仅 IPv6
    Ipv6Only(String),
    /// IPv4 + IPv6 双栈
    DualStack(String, String),
}

impl IpStatus {
    fn has_ip(&self) -> bool {
        !matches!(self, IpStatus::Unexpected | IpStatus::NoIp)
    }
}

enum ConnectionState {
    Disconnected,
    FullStackConfigured,
}

pub async fn start_monitor(config: Config, at_client: ATClient) {
    info!("Starting dial monitor with Disaster Recovery...");
    
    let mut state = ConnectionState::Disconnected;
    let mut ping_fail_count = 0u32;
    let mut unexpected_response_count = 0u32;

    // 订阅 ^NDISSTAT 断开事件，断线时无需等待轮询立即响应
    let ndis_tx = get_ndis_disconnect_tx();
    let mut ndis_rx = ndis_tx.subscribe();

    // 10 秒轮询定时器
    let mut poll_timer = interval(Duration::from_secs(10));
    poll_timer.tick().await; // 消耗第一个立即触发的 tick

    loop {
        // 同时等待：轮询定时器 或 NDIS 断开事件（哪个先到处理哪个）
        let ndis_disconnected = tokio::select! {
            _ = poll_timer.tick() => {
                false // 正常轮询
            }
            result = ndis_rx.recv() => {
                match result {
                    Ok(()) => {
                        warn!("[NDISSTAT] Disconnect event received! Triggering immediate recovery.");
                        true
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("[NDISSTAT] Missed {} disconnect events, treating as disconnected.", n);
                        true
                    }
                    Err(_) => false,
                }
            }
        };

        // NDIS 断开事件直接触发恢复，跳过 IP 检查
        if ndis_disconnected {
            if !matches!(state, ConnectionState::Disconnected) {
                state = ConnectionState::Disconnected;
            }
            trigger_disaster_recovery(&config, &at_client).await;
            ping_fail_count = 0;
            unexpected_response_count = 0;
            continue;
        }

        // 常规轮询：检查 IP 状态
        match check_ip_status(&at_client).await {
            Ok(ip_status) => {
                match ip_status {
                    IpStatus::Unexpected => {
                        unexpected_response_count += 1;
                        warn!("AT+CGPADDR returned unexpected response. Count: {}/3", unexpected_response_count);
                        if unexpected_response_count >= 3 {
                            warn!("3 consecutive unexpected AT responses. Triggering disaster recovery.");
                            trigger_disaster_recovery(&config, &at_client).await;
                            unexpected_response_count = 0;
                            ping_fail_count = 0;
                            state = ConnectionState::Disconnected;
                        }
                    }

                    IpStatus::NoIp => {
                        unexpected_response_count = 0;
                        if !matches!(state, ConnectionState::Disconnected) {
                            warn!("Lost IP address. Resetting state and triggering disaster recovery.");
                            state = ConnectionState::Disconnected;
                        } else {
                            warn!("No IP address detected. Triggering disaster recovery.");
                        }
                        trigger_disaster_recovery(&config, &at_client).await;
                        ping_fail_count = 0;
                        state = ConnectionState::Disconnected;
                    }

                    ref status if status.has_ip() => {
                        unexpected_response_count = 0;
                        log_ip_status(status);

                        match state {
                            ConnectionState::Disconnected => {
                                info!("IP address detected. Starting network setup...");

                                info!("Initializing modem URC reporting configs...");
                                let _ = at_client.send_command("AT+CNMI=2,1,0,2,0".to_string()).await;
                                let _ = at_client.send_command("AT+CMGF=0".to_string()).await;
                                let _ = at_client.send_command("AT+CLIP=1".to_string()).await;

                                let actual_ifname = detect_modem_ifname(&config.advanced_network_config.ifname).await;
                                info!("Auto-detected 5G interface: {}", actual_ifname);

                                if let Err(e) = network::setup_ipv4_only(&config, &actual_ifname).await {
                                    error!("Failed to setup IPv4 network: {}", e);
                                } else {
                                    info!("IPv4 setup done.");
                                }

                                let pdp_type = config.advanced_network_config.pdp_type.to_lowercase();
                                let ipv6_needed = pdp_type.contains("v6") || pdp_type.contains("ipv6");
                                let ipv6_present = matches!(status, IpStatus::Ipv6Only(_) | IpStatus::DualStack(_, _));

                                if ipv6_needed && ipv6_present {
                                    info!("IPv6 is enabled and detected. Injecting IPv6 interface...");
                                    if let Err(e) = network::inject_ipv6_interface(&config, &actual_ifname).await {
                                        error!("Failed to inject IPv6 interface: {}", e);
                                    } else {
                                        info!("IPv6 Injection Completed.");
                                    }
                                } else if ipv6_needed && !ipv6_present {
                                    warn!("IPv6 configured but modem only has IPv4. Skipping IPv6 injection.");
                                }

                                state = ConnectionState::FullStackConfigured;
                                ping_fail_count = 0;
                                info!("Network setup complete. Full stack active.");
                            }

                            ConnectionState::FullStackConfigured => {
                                if !check_ping().await {
                                    ping_fail_count += 1;
                                    warn!("Ping failed in stable state. Count: {}/3", ping_fail_count);
                                    if ping_fail_count >= 3 {
                                        warn!("Continuous 3 ping failures detected! Triggering disaster recovery.");
                                        trigger_disaster_recovery(&config, &at_client).await;
                                        ping_fail_count = 0;
                                        state = ConnectionState::Disconnected;
                                        continue;
                                    }
                                } else {
                                    if ping_fail_count > 0 {
                                        info!("Ping recovered. Resetting failure count.");
                                        ping_fail_count = 0;
                                    }
                                }
                            }
                        }
                    }

                    _ => {}
                }
            }
            Err(e) => {
                warn!("Failed to check IP status: {}. Retrying later.", e);
            }
        }
    }
}

/// 打印当前 IP 状态到日志
fn log_ip_status(status: &IpStatus) {
    match status {
        IpStatus::Ipv4Only(v4) => debug!("Connection status: IPv4 only ({})", v4),
        IpStatus::Ipv6Only(v6) => debug!("Connection status: IPv6 only ({})", v6),
        IpStatus::DualStack(v4, v6) => debug!("Connection status: Dual Stack (v4={}, v6={})", v4, v6),
        _ => {}
    }
}

async fn check_ping() -> bool {
    let output = Command::new("ping")
        .args(&["-c", "1", "-W", "2", "223.5.5.5"])
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            debug!("Ping 223.5.5.5 success.");
            true
        }
        _ => {
            debug!("Ping 223.5.5.5 failed, fallback to 114.114.114.114...");
            let output2 = Command::new("ping")
                .args(&["-c", "1", "-W", "2", "114.114.114.114"])
                .output()
                .await;
            let ok = output2.map(|o| o.status.success()).unwrap_or(false);
            if ok {
                debug!("Ping 114.114.114.114 success.");
            } else {
                warn!("Both ping targets failed.");
            }
            ok
        }
    }
}

/// 检查 IP 状态，返回精细的四状态枚举
/// 参考 QModem modem_dial.sh check_ip() 的 connection_status 设计
async fn check_ip_status(at_client: &ATClient) -> Result<IpStatus> {
    let response = at_client.send_command("AT+CGPADDR".to_string()).await?;

    let content = match response.data {
        Some(c) => c,
        None => {
            warn!("AT+CGPADDR returned no data.");
            return Ok(IpStatus::Unexpected);
        }
    };

    debug!("IP Check Response: {}", content);

    let mut found_v4: Option<String> = None;
    let mut found_v6: Option<String> = None;
    let mut has_cgpaddr_line = false;

    for line in content.lines() {
        let line = line.trim();
        if !line.starts_with("+CGPADDR:") {
            continue;
        }
        has_cgpaddr_line = true;

        let parts: Vec<&str> = line.splitn(2, ':').collect();
        if parts.len() < 2 {
            continue;
        }

        let segments: Vec<&str> = parts[1].split(',').collect();
        // segments[0] 是 PDP 索引，从 [1] 开始是 IP
        for segment in segments.iter().skip(1) {
            let clean_ip = segment.trim_matches(|c| c == '"' || c == ' ' || c == '\r' || c == '\n');

            if clean_ip.is_empty() || clean_ip == "0.0.0.0" || clean_ip == "::" {
                continue;
            }

            if clean_ip.contains('.') && clean_ip.len() <= 15 {
                debug!("Detected IPv4: {}", clean_ip);
                found_v4 = Some(clean_ip.to_string());
            } else if clean_ip.contains(':') && clean_ip.len() <= 39 {
                debug!("Detected IPv6: {}", clean_ip);
                found_v6 = Some(clean_ip.to_string());
            }
        }
    }

    // 如果根本没有 +CGPADDR: 行，视为异常响应
    if !has_cgpaddr_line {
        warn!("AT+CGPADDR response contains no +CGPADDR line: {}", content.replace('\n', " ").replace('\r', " "));
        return Ok(IpStatus::Unexpected);
    }

    Ok(match (found_v4, found_v6) {
        (Some(v4), Some(v6)) => IpStatus::DualStack(v4, v6),
        (Some(v4), None)     => IpStatus::Ipv4Only(v4),
        (None,     Some(v6)) => IpStatus::Ipv6Only(v6),
        (None,     None)     => IpStatus::NoIp,
    })
}

/// MT5700M-CN 专用拨号函数。
/// 
/// 鼎桥 MT5700M-CN PDP 配置固定：
///   CID 1 = 上网业务（CMNET/运营商数据）
///   CID 2 = IMS 业务（VoLTE/短信）
/// 无需动态扫描，直接硬编码激活。
async fn perform_dial(_config: &Config, at_client: &ATClient) -> Result<()> {
    // 检查自动拨号开关，尊重用户在模组上的配置
    let resp_dial = at_client.send_command("AT^SETAUTODIAL?".to_string()).await;
    if let Ok(response) = resp_dial {
        if let Some(content) = response.data {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with("^SETAUTODIAL:") {
                    let parts: Vec<&str> = line.trim_start_matches("^SETAUTODIAL:").split(',').collect();
                    if parts.len() >= 1 && parts[0].trim() == "0" {
                        info!("Auto-dial is disabled in hardware. Aborting perform_dial.");
                        return Ok(());
                    }
                }
            }
        }
    }

    // MT5700M-CN 固定 CID：1=数据, 2=IMS
    info!("[MT5700M-CN] Activating Data PDP (CID=1)...");
    let _ = at_client.send_command("AT+CGACT=1,1".to_string()).await;

    info!("[MT5700M-CN] Activating IMS PDP (CID=2)...");
    let _ = at_client.send_command("AT+CGACT=2,1".to_string()).await;

    Ok(())
}

async fn trigger_disaster_recovery(config: &Config, at_client: &ATClient) {
    info!("=== Phase 1: Disaster Recovery (Soft SIM Plug/Unplug) ===");
    let _ = at_client.send_command("AT^HVSST=1,0".to_string()).await;
    sleep(Duration::from_secs(3)).await;
    let _ = at_client.send_command("AT^HVSST=1,1".to_string()).await;

    let is_cpin_ready = wait_for_cpin_ready(at_client).await;

    if !is_cpin_ready {
        warn!("SIM card failed to recover via AT^HVSST. Triggering Level-2 Recovery (Radio Toggle)...");
        let _ = at_client.send_command("AT+CFUN=4".to_string()).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let _ = at_client.send_command("AT+CFUN=1".to_string()).await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        let final_check = wait_for_cpin_ready(at_client).await;
        if !final_check {
            error!("Level-2 Recovery failed. Modem requires a hard reboot.");
            return;
        }
    }

    info!("=== Phase 2: Configuration & Activation (Dial) ===");
    let auto_dial_resp = at_client.send_command("AT^SETAUTODIAL?".to_string()).await;
    let mut should_dial = true;
    if let Ok(resp) = auto_dial_resp {
        if let Some(data) = resp.data {
            if data.contains("^SETAUTODIAL:0") || data.contains("^SETAUTODIAL: 0") {
                warn!("Auto-dial is DISABLED in modem settings. Aborting dial phase.");
                should_dial = false;
            }
        }
    }

    if should_dial {
        info!("Disconnecting NDIS binding before activation...");
        let _ = at_client.send_command("AT^NDISDUP=1,0".to_string()).await;
        if let Err(e) = perform_dial(config, at_client).await {
            warn!("Dial attempt failed: {}", e);
        }
    }

    // 等待有效 IP，最多 120 秒
    if !wait_for_ip(at_client).await {
        error!("Timed out waiting for IP after dial. Aborting recovery.");
        return;
    }

    info!("=== Phase 3: Bind Data Channel ===");
    let _ = at_client.send_command("AT^NDISDUP=1,1".to_string()).await;
    info!("Waiting for modem DHCP server to be ready...");
    sleep(Duration::from_secs(3)).await;

    info!("Restarting physical link...");
    let ifname = &config.advanced_network_config.ifname;
    let actual_ifname = detect_modem_ifname(ifname).await;

    match Command::new("ip").args(&["link", "set", "dev", &actual_ifname, "down"]).status().await {
        Ok(_) => info!("Interface {} brought down", actual_ifname),
        Err(e) => error!("Failed to down {}: {}", actual_ifname, e),
    }
    sleep(Duration::from_secs(1)).await;
    match Command::new("ip").args(&["link", "set", "dev", &actual_ifname, "up"]).status().await {
        Ok(_) => info!("Data channel bound. Link restarted!"),
        Err(e) => error!("Failed to up {}: {}", actual_ifname, e),
    }
}

async fn wait_for_cpin_ready(at_client: &ATClient) -> bool {
    info!("Waiting for SIM card to be READY...");
    let mut retries = 0;
    let max_retries = 15; // 最多等 30 秒 (15 * 2s)

    loop {
        if retries >= max_retries {
            error!("Timeout waiting for SIM card (30s). The modem's baseband might be stuck!");
            return false;
        }

        match at_client.send_command("AT+CPIN?".to_string()).await {
            Ok(resp) => {
                if let Some(data) = resp.data {
                    let clean_data = data.replace('\n', " ").replace('\r', " ");
                    info!("CPIN Check (Retry {}): {}", retries + 1, clean_data);
                    if clean_data.contains("+CPIN: READY") || clean_data.contains("+CPIN:READY") {
                        info!("SIM Card is READY.");
                        return true;
                    } else if clean_data.contains("ERROR") || clean_data.contains("NOT INSERTED") {
                        warn!("Abnormal SIM state detected!");
                    }
                } else {
                    warn!("CPIN returned OK but with empty data.");
                }
            }
            Err(e) => {
                error!("Failed to send AT+CPIN? command: {}", e);
            }
        }

        retries += 1;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// 等待有效 IP，参考 QModem 增加 120 秒超时熔断
/// 返回 true 表示成功获取 IP，false 表示超时
async fn wait_for_ip(at_client: &ATClient) -> bool {
    info!("Waiting for valid IP address (timeout: 120s)...");
    let max_retries = 60u32; // 60 * 2s = 120s
    let mut retries = 0u32;

    loop {
        if retries >= max_retries {
            error!("Timed out (120s) waiting for valid IP address.");
            return false;
        }
        match check_ip_status(at_client).await {
            Ok(status) if status.has_ip() => {
                info!("IP successfully obtained.");
                return true;
            }
            Ok(IpStatus::Unexpected) => {
                warn!("Unexpected AT response while waiting for IP (retry {}/{}).", retries + 1, max_retries);
            }
            Ok(_) => {
                debug!("No IP yet, retrying ({}/{})...", retries + 1, max_retries);
            }
            Err(e) => {
                warn!("Error checking IP status: {}", e);
            }
        }
        retries += 1;
        sleep(Duration::from_secs(2)).await;
    }
}

async fn detect_modem_ifname(configured: &str) -> String {
    if !configured.is_empty() && configured != "auto" {
        return configured.to_string();
    }
    if let Some(iface) = detect_modem_interface().await {
        return iface;
    }
    "usb0".to_string()
}

/// 基于 QModem 原理的绝对精准探测法：直接读取 USB 设备的 Vendor ID (厂商代码)
async fn detect_modem_interface() -> Option<String> {
    let net_dir = "/sys/class/net";
    let Ok(mut entries) = fs::read_dir(net_dir).await else { return None; };

    // MT5700M-CN 专用：只匹配鼎桥 VID 3466
    let valid_vids = ["3466"];

    while let Ok(Some(entry)) = entries.next_entry().await {
        let iface = entry.file_name().into_string().unwrap_or_default();
        if iface == "lo" || iface.starts_with("br-") || iface.starts_with("wl") || iface.starts_with("ra") {
            continue;
        }

        let vendor_path_direct = format!("{}/{}/device/idVendor", net_dir, iface);
        let vendor_path_parent = format!("{}/{}/device/../idVendor", net_dir, iface);

        let mut vid = match fs::read_to_string(&vendor_path_direct).await {
            Ok(v) => v,
            Err(_) => String::new(),
        };
        if vid.trim().is_empty() {
            if let Ok(v) = fs::read_to_string(&vendor_path_parent).await {
                vid = v;
            }
        }

        let vid = vid.trim().to_lowercase();
        if !vid.is_empty() && valid_vids.contains(&vid.as_str()) {
            info!("Hardware probing success! Found 5G modem: {} (Vendor ID: {})", iface, vid);
            return Some(iface);
        }
    }

    warn!("No valid 5G/4G USB modem interface found based on Vendor ID.");
    None
} 
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
                                // ipv6_needed：配置了 v6 协议类型（ipv4v6 / ipv6）
                                // 注意：不依赖 ipv6_present（AT+CGPADDR 可能只返回数据 PDP 的 IPv4，
                                // IMS/IPv6 地址不一定出现在响应中），只要配置了就尝试注入
                                let ipv6_needed = pdp_type.contains("v6") || pdp_type.contains("ipv6");

                                if ipv6_needed {
                                    info!("IPv6 configured (pdp_type={}). Injecting IPv6 interface...", pdp_type);
                                    if let Err(e) = network::inject_ipv6_interface(&config, &actual_ifname).await {
                                        error!("Failed to inject IPv6 interface: {}", e);
                                    } else {
                                        info!("IPv6 Injection Completed.");
                                    }
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

            // MT5700M-CN 的 IPv6 地址以点分十进制格式返回（16个字节，共15个点）
            // 例如: "32.8.0.2.0.2.0.1.255.255.255.255.255.255.255.255"
            // 标准冒号格式: "2001:db8::1" 也兼容处理
            let dot_count = clean_ip.chars().filter(|&c| c == '.').count();
            let colon_count = clean_ip.chars().filter(|&c| c == ':').count();

            if colon_count >= 2 {
                // 标准 IPv6 冒号格式
                debug!("Detected IPv6 (colon fmt): {}", clean_ip);
                found_v6 = Some(clean_ip.to_string());
            } else if dot_count == 15 {
                // MT5700M-CN 点分十进制 IPv6 格式（16字节，15个点）
                // 验证所有段都是 0-255 的数字
                let all_valid = clean_ip.split('.').all(|s| s.parse::<u8>().is_ok());
                if all_valid {
                    debug!("Detected IPv6 (dotted-decimal fmt): {}", clean_ip);
                    found_v6 = Some(clean_ip.to_string());
                } else {
                    debug!("Detected IPv4: {}", clean_ip);
                    found_v4 = Some(clean_ip.to_string());
                }
            } else if clean_ip.contains('.') && dot_count == 3 {
                // 标准 IPv4 格式（x.x.x.x）
                debug!("Detected IPv4: {}", clean_ip);
                found_v4 = Some(clean_ip.to_string());
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

/// 四级灾难恢复入口（基于 MT5700M-CN AT命令手册）
///
/// L1: AT^HVSST=1,0/1   SIM 软拔插     → SIM 接触/识别异常
/// L2: AT+CFUN=4/1      射频 offline   → 射频/NAS 层卡死
/// L3: AT+CGATT=0/1     PS DETACH      → EPS Bearer 卡死（注网有但无IP）
/// L4: AT+CFUN=1,1      模组复位       → 所有软恢复失败的最后手段
async fn trigger_disaster_recovery(config: &Config, at_client: &ATClient) {
    // ── Level-1: SIM 软拔插 ──────────────────────────────────────────────────
    info!("[L1] SIM soft replug (AT^HVSST=1,0 -> AT^HVSST=1,1)");
    let _ = at_client.send_command("AT^HVSST=1,0".to_string()).await;
    sleep(Duration::from_secs(3)).await;
    let _ = at_client.send_command("AT^HVSST=1,1".to_string()).await;

    if wait_for_cpin_ready(at_client).await {
        info!("[L1] SIM READY. Attempting dial...");
        if try_dial_and_bind(config, at_client).await {
            return;
        }
        // 拨号后超时无IP：判断是否 EPS Bearer 卡死，是则直接升 L3
        if check_registration(at_client).await {
            warn!("[L1] Registered but no IP (EPS Bearer stuck). Escalating to L3.");
            if level3_ps_detach(config, at_client).await { return; }
            level4_reset(at_client).await;
            return;
        }
    }

    // ── Level-2: 射频 Offline/Online ─────────────────────────────────────────
    // 手册：CFUN=0/1 间隔至少 1s；CFUN=4 = offline，CFUN=1 = online
    warn!("[L2] Radio toggle (AT+CFUN=4 -> AT+CFUN=1)");
    let _ = at_client.send_command("AT+CFUN=4".to_string()).await;
    sleep(Duration::from_secs(3)).await;
    let _ = at_client.send_command("AT+CFUN=1".to_string()).await;
    sleep(Duration::from_secs(5)).await;

    if wait_for_cpin_ready(at_client).await {
        info!("[L2] SIM READY after radio toggle. Attempting dial...");
        if try_dial_and_bind(config, at_client).await {
            return;
        }
        if check_registration(at_client).await {
            warn!("[L2] Registered but no IP after radio toggle. Escalating to L3.");
            if level3_ps_detach(config, at_client).await { return; }
        }
    }

    // ── Level-4: 模组复位 ─────────────────────────────────────────────────────
    level4_reset(at_client).await;
}

/// Level-3: PS 域强制 DETACH/ATTACH
/// 手册说明：去附着时所有激活的 PDP 上下文自动失效，清除 EPS Bearer Context
async fn level3_ps_detach(config: &Config, at_client: &ATClient) -> bool {
    warn!("[L3] PS DETACH/ATTACH (AT+CGATT=0 -> AT+CGATT=1)");
    let _ = at_client.send_command("AT+CGATT=0".to_string()).await;
    sleep(Duration::from_secs(5)).await;
    let _ = at_client.send_command("AT+CGATT=1".to_string()).await;
    sleep(Duration::from_secs(8)).await;
    info!("[L3] Re-dialing after PS ATTACH...");
    try_dial_and_bind(config, at_client).await
}

/// Level-4: 模组复位
/// 手册：AT+CFUN=1,1 在 online 模式下触发复位；AT^RESET 作为备用
async fn level4_reset(at_client: &ATClient) {
    error!("[L4] Modem reset (AT+CFUN=1,1)");
    let ok = at_client.send_command("AT+CFUN=1,1".to_string()).await
        .map(|r| r.success).unwrap_or(false);
    if !ok {
        warn!("[L4] AT+CFUN=1,1 failed, fallback to AT^RESET");
        let _ = at_client.send_command("AT^RESET".to_string()).await;
    }
    info!("[L4] Waiting 35s for modem reboot...");
    sleep(Duration::from_secs(35)).await;
}

/// 执行拨号并绑定数据通道，返回 true 表示成功
async fn try_dial_and_bind(config: &Config, at_client: &ATClient) -> bool {
    let auto_dial_resp = at_client.send_command("AT^SETAUTODIAL?".to_string()).await;
    let mut should_dial = true;
    if let Ok(resp) = auto_dial_resp {
        if let Some(data) = resp.data {
            // 手册注意：响应前缀可能是 ^SETAUTODAIL（手册原文拼写）或 ^SETAUTODIAL
            // 格式：^SETAUTODAIL:<enable>,<dial_mode>,...  或  ^SETAUTODIAL:<enable>
            let disabled = data.lines().any(|line| {
                let l = line.trim();
                (l.starts_with("^SETAUTODIAL:") || l.starts_with("^SETAUTODAIL:"))
                    && l.split(':').nth(1).map(|v| v.split(',').next().map(|e| e.trim() == "0").unwrap_or(false)).unwrap_or(false)
            });
            if disabled {
                warn!("[dial] Auto-dial disabled in modem. Skipping.");
                should_dial = false;
            }
        }
    }
    if should_dial {
        // 手册：NDISDUP 是异步AT，断开后需等待 ^NDISSTAT: 0 确认，此处用 sleep 兜底
        let _ = at_client.send_command("AT^NDISDUP=1,0".to_string()).await;
        sleep(Duration::from_secs(2)).await;
        if let Err(e) = perform_dial(config, at_client).await {
            warn!("[dial] perform_dial failed: {}", e);
        }
    }
    if !wait_for_ip(at_client).await {
        warn!("[dial] Timed out waiting for IP.");
        return false;
    }
    info!("[dial] IP obtained. Binding NDIS channel...");
    // 手册：NDISDUP 是异步AT，OK 只代表发送成功，实际连接由 ^NDISSTAT: 1 确认
    // 此处 sleep 5s 等待 ^NDISSTAT 上报及 DHCP 就绪
    let _ = at_client.send_command("AT^NDISDUP=1,1".to_string()).await;
    sleep(Duration::from_secs(5)).await;
    let actual_ifname = detect_modem_ifname(&config.advanced_network_config.ifname).await;
    let _ = Command::new("ip").args(&["link", "set", "dev", &actual_ifname, "down"]).status().await;
    sleep(Duration::from_secs(1)).await;
    let _ = Command::new("ip").args(&["link", "set", "dev", &actual_ifname, "up"]).status().await;
    info!("[dial] Interface {} restarted. Recovery complete.", actual_ifname);
    true
}

/// 检查模组是否已注网（CS/PS 域）
/// 返回 true 表示已注网但可能无数据承载（EPS Bearer 卡死场景）
async fn check_registration(at_client: &ATClient) -> bool {
    // 检查 LTE/5G PS 域注网状态
    if let Ok(resp) = at_client.send_command("AT+CEREG?".to_string()).await {
        if let Some(data) = resp.data {
            // +CEREG: 0,1 或 +CEREG: 0,5 表示已注网
            if data.contains(",1") || data.contains(",5") {
                info!("[check_registration] CEREG: registered.");
                return true;
            }
        }
    }
    // 回退检查 CREG
    if let Ok(resp) = at_client.send_command("AT+CREG?".to_string()).await {
        if let Some(data) = resp.data {
            if data.contains(",1") || data.contains(",5") {
                info!("[check_registration] CREG: registered.");
                return true;
            }
        }
    }
    warn!("[check_registration] Not registered.");
    false
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
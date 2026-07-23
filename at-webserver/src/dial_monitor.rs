use crate::client::ATClient;
use crate::config::Config;
use crate::models::get_ndis_disconnect_tx;
use crate::network;
use log::{info, warn, error, debug};
use std::time::Duration;
use tokio::time::{sleep, interval};
use tokio::process::Command;
use anyhow::{anyhow, bail, Result};

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
    DataPathConfigured,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DataInterface {
    device: String,
    logical_interface: String,
    managed: bool,
}

impl DataInterface {
    fn managed(device: String) -> Self {
        Self {
            device,
            logical_interface: "wan_modem".to_string(),
            managed: true,
        }
    }

    fn existing(logical_interface: &str, device: String) -> Self {
        Self {
            device,
            logical_interface: logical_interface.to_string(),
            managed: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataMode {
    Usb,
    Ethernet,
}

impl DataMode {
    fn label(self) -> &'static str {
        match self {
            Self::Usb => "USB virtual network interface",
            Self::Ethernet => "Ethernet port",
        }
    }
}

pub async fn start_monitor(config: Config, at_client: ATClient) {
    info!("Starting dial monitor with Disaster Recovery...");
    
    let mut state = ConnectionState::Disconnected;
    let mut ping_fail_count = 0u32;
    let mut unexpected_response_count = 0u32;
    let mut data_mode: Option<DataMode> = None;
    let mut active_interface: Option<DataInterface> = None;

    // 订阅 ^NDISSTAT 断开事件，断线时无需等待轮询立即响应
    let ndis_tx = get_ndis_disconnect_tx();
    let mut ndis_rx = ndis_tx.subscribe();

    // 10 秒轮询定时器
    let mut poll_timer = interval(Duration::from_secs(10));

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

        // The modem explicitly reports whether data uses a USB virtual NIC or
        // the external Ethernet port. Refresh before every health check so a
        // modem reboot or mode switch cannot leave us managing the wrong interface.
        match detect_data_mode(&at_client).await {
            Ok(detected) => {
                if data_mode != Some(detected) {
                    if let Some(previous) = data_mode {
                        warn!(
                            "Modem data mode changed: {} -> {}.",
                            previous.label(),
                            detected.label()
                        );
                        let _ = network::teardown_modem_network().await;
                    } else {
                        info!("Detected modem data mode: {}.", detected.label());
                    }
                    data_mode = Some(detected);
                    state = ConnectionState::Disconnected;
                    active_interface = None;
                }
            }
            Err(e) => {
                warn!("Unable to determine modem data mode: {}", e);
                if data_mode.is_none() {
                    continue;
                }
            }
        }

        let Some(active_mode) = data_mode else {
            continue;
        };

        // NDIS 断开事件直接触发恢复，跳过 IP 检查
        if ndis_disconnected {
            // 检查用户是否手动关闭了自动拨号，若关闭则不进行灾难恢复
            if is_auto_dial_disabled(&at_client).await {
                debug!("[monitor] NDIS disconnected but AT^SETAUTODIAL=0 (user disabled). Skipping recovery.");
                state = ConnectionState::Disconnected;
                ping_fail_count = 0;
                unexpected_response_count = 0;
                active_interface = None;
                continue;
            }
            if !matches!(state, ConnectionState::Disconnected) {
                state = ConnectionState::Disconnected;
            }
            trigger_disaster_recovery(&config, &at_client, active_mode).await;
            ping_fail_count = 0;
            unexpected_response_count = 0;
            active_interface = None;
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
                            trigger_disaster_recovery(&config, &at_client, active_mode).await;
                            unexpected_response_count = 0;
                            ping_fail_count = 0;
                            state = ConnectionState::Disconnected;
                            active_interface = None;
                        }
                    }

                    IpStatus::NoIp => {
                        unexpected_response_count = 0;
                        // 检查用户是否手动关闭了自动拨号，若关闭则不触发灾难恢复
                        if is_auto_dial_disabled(&at_client).await {
                            debug!("[monitor] No IP but AT^SETAUTODIAL=0 (user disabled). Skipping recovery.");
                            state = ConnectionState::Disconnected;
                            active_interface = None;
                            continue;
                        }
                        if !matches!(state, ConnectionState::Disconnected) {
                            warn!("Lost IP address. Resetting state and triggering disaster recovery.");
                        } else {
                            warn!("No IP address detected. Triggering disaster recovery.");
                        }
                        trigger_disaster_recovery(&config, &at_client, active_mode).await;
                        ping_fail_count = 0;
                        state = ConnectionState::Disconnected;
                        active_interface = None;
                    }

                    ref status if status.has_ip() => {
                        unexpected_response_count = 0;
                        log_ip_status(status);

                        match state {
                            ConnectionState::Disconnected => {
                                info!("IP address detected. Starting network setup...");

                                debug!("Initializing modem URC reporting configs...");
                                let _ = at_client.send_command("AT+CNMI=2,1,0,2,0".to_string()).await;
                                let _ = at_client.send_command("AT+CMGF=0".to_string()).await;
                                let _ = at_client.send_command("AT+CLIP=1".to_string()).await;
                                // 手册：AT+CPMS 的 mem3（接收存储）掉电不保存，重启后重置
                                // mem1/mem2 上电后与上次 mem3 保持一致，因此三者都需重新下发
                                let sms_mem = &config.advanced_network_config.sms_storage;
                                let cpms_cmd = format!("AT+CPMS=\"{}\",\"{}\",\"{}\"", sms_mem, sms_mem, sms_mem);
                                debug!("Setting SMS storage to {} (AT+CPMS)...", sms_mem);
                                let _ = at_client.send_command(cpms_cmd).await;

                                let selected_interface = match detect_data_interface(
                                    &config.advanced_network_config.ifname,
                                    active_mode,
                                ).await {
                                    Ok(selection) => selection,
                                    Err(e) => {
                                        error!("Failed to select data interface: {}", e);
                                        state = ConnectionState::Disconnected;
                                        active_interface = None;
                                        continue;
                                    }
                                };
                                info!(
                                    "Using data device {} through OpenWrt interface {} for {} mode.",
                                    selected_interface.device,
                                    selected_interface.logical_interface,
                                    active_mode.label(),
                                );

                                let setup_result = if selected_interface.managed {
                                    network::setup_ipv4_only(&config, &selected_interface.device).await
                                } else {
                                    network::ensure_existing_ipv4_interface(
                                        &selected_interface.logical_interface,
                                    ).await
                                };
                                if let Err(e) = setup_result {
                                    error!("Failed to setup IPv4 network: {}", e);
                                    state = ConnectionState::Disconnected;
                                    active_interface = None;
                                    continue;
                                }
                                debug!("IPv4 setup done.");

                                let pdp_type = config.advanced_network_config.pdp_type.to_lowercase();
                                // ipv6_needed：配置了 v6 协议类型（ipv4v6 / ipv6）
                                // 注意：不依赖 ipv6_present（AT+CGPADDR 可能只返回数据 PDP 的 IPv4，
                                // IMS/IPv6 地址不一定出现在响应中），只要配置了就尝试注入
                                let ipv6_needed = pdp_type.contains("v6") || pdp_type.contains("ipv6");

                                if ipv6_needed && selected_interface.managed {
                                    info!("IPv6 configured (pdp_type={}). Injecting IPv6 interface...", pdp_type);
                                    if let Err(e) = network::inject_ipv6_interface(&config, &selected_interface.device).await {
                                        error!("Failed to inject IPv6 interface: {}", e);
                                    } else {
                                        debug!("IPv6 Injection Completed.");
                                    }
                                } else if ipv6_needed {
                                    debug!(
                                        "Reusing existing OpenWrt interface {}; preserving its IPv6 configuration.",
                                        selected_interface.logical_interface
                                    );
                                }

                                active_interface = Some(selected_interface.clone());
                                state = ConnectionState::DataPathConfigured;
                                ping_fail_count = 0;
                                info!(
                                    "Network setup complete. Router data path is active on {} (device {}).",
                                    selected_interface.logical_interface,
                                    selected_interface.device,
                                );
                            }

                            ConnectionState::DataPathConfigured => {
                                let Some(logical_interface) = active_interface
                                    .as_ref()
                                    .map(|selection| selection.logical_interface.clone())
                                else {
                                    state = ConnectionState::Disconnected;
                                    continue;
                                };
                                if !check_router_network_status(&logical_interface).await {
                                    ping_fail_count += 1;
                                    warn!(
                                        "Router-side network check failed on {}. Count: {}/3",
                                        logical_interface,
                                        ping_fail_count
                                    );
                                    if ping_fail_count >= 3 {
                                        warn!("Continuous 3 router-side failures detected! Triggering disaster recovery.");
                                        trigger_disaster_recovery(&config, &at_client, active_mode).await;
                                        ping_fail_count = 0;
                                        state = ConnectionState::Disconnected;
                                        active_interface = None;
                                        continue;
                                    }
                                } else if ping_fail_count > 0 {
                                    debug!("Router-side network recovered. Resetting failure count.");
                                    ping_fail_count = 0;
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

async fn check_router_network_status(interface: &str) -> bool {
    // 1) 检查实际使用的 OpenWrt 逻辑接口状态（路由侧）
    let status_out = Command::new("ifstatus")
        .arg(interface)
        .output()
        .await;

    let output = match status_out {
        Ok(o) if o.status.success() => o,
        _ => {
            warn!("ifstatus {} failed.", interface);
            return false;
        }
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(j) => j,
        Err(e) => {
            warn!("Failed to parse ifstatus {} JSON: {}", interface, e);
            return false;
        }
    };

    let up = v.get("up").and_then(|x| x.as_bool()).unwrap_or(false);
    if !up {
        warn!("{} is down.", interface);
        return false;
    }

    // 2) 需要有 IPv4 地址，且有默认路由（target=0.0.0.0）
    let has_ipv4 = v.get("ipv4-address")
        .and_then(|x| x.as_array())
        .map(|arr| !arr.is_empty())
        .unwrap_or(false);

    let has_default_route = v.get("route")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter().any(|r| {
                r.get("target")
                    .and_then(|t| t.as_str())
                    .map(|t| t == "0.0.0.0")
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    if has_ipv4 && has_default_route {
        debug!(
            "Router-side network status OK ({} up + IPv4 + default route).",
            interface
        );
        true
    } else {
        warn!(
            "Router-side network status not ready: ipv4={}, default_route={}",
            has_ipv4,
            has_default_route
        );
        false
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
/// 设计原则：后端不主动修改或激活 PDP 上下文。
/// PDP 配置（AT+CGDCONT）和激活（AT+CGACT）由用户在前端完成，
/// 模组内部已保存的 PDP 数据会在 NDISDUP 时自动使用。
/// 后端只负责建立 NDIS 数据通道（AT^NDISDUP=1,1）。
async fn perform_dial(_config: &Config, at_client: &ATClient) -> Result<()> {
    // 手册：NDISDUP 是异步AT，OK 只代表发送成功
    // 实际连接建立由 ^NDISSTAT: 1 URC 确认
    info!("[dial] Establishing NDIS data channel (AT^NDISDUP=1,1)...");
    let _ = at_client.send_command("AT^NDISDUP=1,1".to_string()).await;
    Ok(())
}

/// 检查模组是否已被用户手动关闭自动拨号（AT^SETAUTODIAL=0）
/// 返回 true 表示已关闭，后端不应触发灾难恢复
async fn is_auto_dial_disabled(at_client: &ATClient) -> bool {
    let resp = at_client.send_command("AT^SETAUTODIAL?".to_string()).await;
    if let Ok(r) = resp {
        if let Some(data) = r.data {
            // Some firmware spells the prefix as SETAUTODAIL.
            return parse_auto_dial_enabled(&data)
                .map(|enabled| enabled == 0)
                .unwrap_or(false);
        }
    }
    false // 查询失败时保守处理，不阻止恢复
}

/// 精简灾难恢复入口（只保留最有效、最快路径）
///
/// 快速恢复步骤：
/// 1) 重建 NDIS 通道（AT^NDISDUP=1,0 -> AT^NDISDUP=1,1）
/// 2) 重启路由侧网卡（ip link down/up）
///
/// 不再执行 HVSST/CFUN/CGATT/模组复位等慢恢复流程。
async fn trigger_disaster_recovery(
    config: &Config,
    at_client: &ATClient,
    data_mode: DataMode,
) {
    warn!("[FAST-RECOVERY] Rebuilding NDIS channel and restarting interface...");

    if try_dial_and_bind(config, at_client, data_mode).await {
        info!("[FAST-RECOVERY] Recovery succeeded.");
    } else {
        warn!("[FAST-RECOVERY] Recovery failed this round; will retry on next monitor cycle.");
    }
}

/// 执行拨号并绑定数据通道，返回 true 表示成功
/// 注意：调用此函数前已确认 SETAUTODIAL != 0
async fn try_dial_and_bind(
    config: &Config,
    at_client: &ATClient,
    data_mode: DataMode,
) -> bool {
    // 断开旧 NDIS 连接
    // 手册：NDISDUP 是异步AT，断开后需等待 ^NDISSTAT: 0，此处用 sleep 兜底
    let _ = at_client.send_command("AT^NDISDUP=1,0".to_string()).await;
    sleep(Duration::from_secs(2)).await;

    // perform_dial 只建立 NDIS 通道，PDP 由模组内部数据驱动
    if let Err(e) = perform_dial(config, at_client).await {
        warn!("[dial] perform_dial failed: {}", e);
    }

    if !wait_for_ip(at_client).await {
        warn!("[dial] Timed out waiting for IP.");
        return false;
    }
    info!("[dial] IP obtained. Binding NDIS channel...");
    // 手册：NDISDUP 是异步AT，sleep 5s 等待 ^NDISSTAT: 1 及 DHCP 就绪
    let _ = at_client.send_command("AT^NDISDUP=1,1".to_string()).await;
    sleep(Duration::from_secs(5)).await;
    let selected_interface = match detect_data_interface(
        &config.advanced_network_config.ifname,
        data_mode,
    ).await {
        Ok(selection) => selection,
        Err(e) => {
            error!("[dial] Failed to select data interface: {}", e);
            return false;
        }
    };
    let _ = Command::new("ip").args(&["link", "set", "dev", &selected_interface.device, "down"]).status().await;
    sleep(Duration::from_secs(1)).await;
    let _ = Command::new("ip").args(&["link", "set", "dev", &selected_interface.device, "up"]).status().await;
    info!(
        "[dial] Device {} restarted for OpenWrt interface {}. Recovery complete.",
        selected_interface.device,
        selected_interface.logical_interface
    );
    true
}

/// 等待有效 IP，参考 QModem 增加 120 秒超时熔断
/// 返回 true 表示成功获取 IP，false 表示超时
async fn wait_for_ip(at_client: &ATClient) -> bool {
    debug!("Waiting for valid IP address (timeout: 120s)...");
    let max_retries = 60u32; // 60 * 2s = 120s
    let mut retries = 0u32;

    loop {
        if retries >= max_retries {
            error!("Timed out (120s) waiting for valid IP address.");
            return false;
        }
        match check_ip_status(at_client).await {
            Ok(status) if status.has_ip() => {
                debug!("IP successfully obtained.");
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

async fn detect_data_interface(configured: &str, data_mode: DataMode) -> Result<DataInterface> {
    if !configured.is_empty() && configured != "auto" {
        if let Some(selection) = resolve_logical_interface(configured).await {
            return Ok(selection);
        }

        let path = format!("/sys/class/net/{}", configured);
        if fs::metadata(&path).await.is_err() {
            bail!("configured interface {} does not exist", configured);
        }
        return Ok(DataInterface::managed(configured.to_string()));
    }

    match data_mode {
        DataMode::Usb => detect_usb_modem_interface()
            .await
            .map(DataInterface::managed)
            .ok_or_else(|| {
                anyhow!("no USB virtual network interface with vendor ID 3466 was found")
            }),
        DataMode::Ethernet => {
            if let Some(selection) = resolve_logical_interface("wan").await {
                info!(
                    "Using native OpenWrt WAN interface {} (device {}).",
                    selection.logical_interface,
                    selection.device
                );
                return Ok(selection);
            }

            detect_native_gigabit_interface()
                .await
                .map(DataInterface::managed)
        }
    }
}

/// Resolve an existing OpenWrt logical interface to its kernel network device.
/// Reusing `wan` avoids starting a second DHCP client on the same physical port.
async fn resolve_logical_interface(interface: &str) -> Option<DataInterface> {
    let uci_key = format!("network.{}.device", interface);
    let configured_device = Command::new("uci")
        .args(["-q", "get", &uci_key])
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (!value.is_empty()).then_some(value)
        });

    if let Some(device) = configured_device {
        if fs::metadata(format!("/sys/class/net/{}", device)).await.is_ok() {
            return Some(DataInterface::existing(interface, device));
        }
    }

    let output = Command::new("ifstatus").arg(interface).output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let selection = interface_status_devices(&status).find_map(|device| {
        std::path::Path::new("/sys/class/net")
            .join(&device)
            .exists()
            .then(|| DataInterface::existing(interface, device))
    });
    selection
}

fn interface_status_devices(status: &serde_json::Value) -> impl Iterator<Item = String> + '_ {
    ["device", "l3_device"]
        .into_iter()
        .filter_map(|field| status.get(field)?.as_str().map(str::to_string))
}

/// USB data mode: find the virtual ECM/NCM interface below the modem USB device.
async fn detect_usb_modem_interface() -> Option<String> {
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
            info!("Found USB modem data interface: {} (Vendor ID: {})", iface, vid);
            return Some(iface);
        }
    }

    warn!("No USB modem data interface found based on Vendor ID.");
    None
}

/// Fallback for systems without a resolvable `network.wan`: select a unique,
/// linked native gigabit port. Multi-WAN systems must configure the interface.
async fn detect_native_gigabit_interface() -> Result<String> {
    let net_dir = "/sys/class/net";
    let mut entries = fs::read_dir(net_dir).await?;
    let mut candidates = Vec::new();

    while let Some(entry) = entries.next_entry().await? {
        let iface = entry.file_name().into_string().unwrap_or_default();
        if should_skip_interface(&iface) {
            continue;
        }

        let base = format!("{}/{}", net_dir, iface);
        if fs::metadata(format!("{}/device", base)).await.is_err()
            || fs::metadata(format!("{}/master", base)).await.is_ok()
        {
            continue;
        }

        let device_path = match fs::canonicalize(format!("{}/device", base)).await {
            Ok(path) => path.to_string_lossy().to_lowercase(),
            Err(_) => continue,
        };
        if device_path.contains("/usb") {
            continue;
        }

        let carrier = fs::read_to_string(format!("{}/carrier", base))
            .await
            .unwrap_or_default();
        if carrier.trim() != "1" {
            continue;
        }

        let speed = fs::read_to_string(format!("{}/speed", base))
            .await
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok());
        if speed != Some(1000) {
            continue;
        }

        debug!(
            "Native gigabit Ethernet candidate: {} (speed=1000, path={})",
            iface,
            device_path
        );
        candidates.push(iface);
    }

    select_single_ethernet_candidate(candidates)
}

fn should_skip_interface(iface: &str) -> bool {
    iface == "lo"
        || iface.starts_with("br-")
        || iface.starts_with("wl")
        || iface.starts_with("ra")
        || iface.starts_with("usb")
        || iface.starts_with("wwan")
        || iface.starts_with("ppp")
        || iface.starts_with("tun")
        || iface.starts_with("tap")
        || iface.starts_with("veth")
        || iface.starts_with("docker")
}

fn select_single_ethernet_candidate(mut candidates: Vec<String>) -> Result<String> {
    candidates.sort();
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => bail!(
            "native OpenWrt interface wan could not be resolved and no active native gigabit port was found; configure option ifname explicitly"
        ),
        _ => bail!(
            "multiple active native gigabit Ethernet ports were found ({}); configure the modem WAN interface explicitly",
            candidates.join(", ")
        ),
    }
}

async fn detect_data_mode(at_client: &ATClient) -> Result<DataMode> {
    let response = at_client
        .send_command("AT^SETAUTODIAL?".to_string())
        .await?;
    if !response.success {
        bail!(
            "AT^SETAUTODIAL? failed: {}",
            response.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }

    let data = response
        .data
        .ok_or_else(|| anyhow!("AT^SETAUTODIAL? returned no data"))?;
    parse_data_mode(&data).ok_or_else(|| {
        anyhow!(
            "unsupported or malformed AT^SETAUTODIAL? response: {}",
            data.replace(['\r', '\n'], " ")
        )
    })
}

fn find_auto_dial_payload(data: &str) -> Option<&str> {
    data.lines().find_map(|line| {
        let line = line.trim();
        Some(line
            .strip_prefix("^SETAUTODIAL:")
            .or_else(|| line.strip_prefix("^SETAUTODAIL:"))?
            .trim())
    })
}

fn parse_auto_dial_enabled(data: &str) -> Option<u8> {
    find_auto_dial_payload(data)?
        .split(',')
        .next()?
        .trim()
        .parse::<u8>()
        .ok()
}

fn parse_auto_dial_fields(data: &str) -> Option<(u8, u8)> {
    let mut fields = find_auto_dial_payload(data)?.split(',').map(str::trim);
    let enabled = fields.next()?.parse::<u8>().ok()?;
    let mode = fields.next()?.parse::<u8>().ok()?;
    Some((enabled, mode))
}

fn parse_data_mode(data: &str) -> Option<DataMode> {
    match parse_auto_dial_fields(data)?.1 {
        1 => Some(DataMode::Usb),
        2 => Some(DataMode::Ethernet),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usb_data_mode() {
        assert_eq!(
            parse_data_mode("^SETAUTODIAL: 1,1,\"IPV4V6\"\r\nOK"),
            Some(DataMode::Usb)
        );
    }

    #[test]
    fn parses_ethernet_data_mode_and_firmware_typo() {
        assert_eq!(
            parse_data_mode("^SETAUTODAIL:0,2\r\nOK"),
            Some(DataMode::Ethernet)
        );
    }

    #[test]
    fn rejects_unknown_data_mode() {
        assert_eq!(parse_data_mode("^SETAUTODIAL: 1,9\r\nOK"), None);
    }

    #[test]
    fn parses_disabled_single_field_response() {
        assert_eq!(parse_auto_dial_enabled("^SETAUTODAIL: 0\r\nOK"), Some(0));
    }

    #[test]
    fn rejects_ambiguous_native_ethernet_candidates() {
        let result = select_single_ethernet_candidate(vec![
            "eth2".to_string(),
            "eth3".to_string(),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn prefers_physical_device_over_protocol_l3_device() {
        let status = serde_json::json!({
            "device": "eth1",
            "l3_device": "pppoe-wan"
        });
        assert_eq!(
            interface_status_devices(&status).collect::<Vec<_>>(),
            vec!["eth1".to_string(), "pppoe-wan".to_string()]
        );
    }
}

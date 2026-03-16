use crate::client::ATClient;
use crate::config::Config;
use crate::network;
use log::{info, warn, error, debug};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tokio::process::Command;
use anyhow::Result;

use tokio::fs;

enum ConnectionState {
    Disconnected,
    IPv4Configured(Instant),
    FullStackConfigured,
}

pub async fn start_monitor(config: Config, at_client: ATClient) {
    info!("Starting dial monitor with Delayed IPv6 Injection...");
    
    // Track connection state
    let mut state = ConnectionState::Disconnected;

    loop {
        // Check IP status
        match check_ip_status(&at_client).await {
            Ok(has_ip) => {
                if has_ip {
                    match state {
                        ConnectionState::Disconnected => {
                            info!("IP address detected. Starting IPv4 setup...");
                            
                            info!("Initializing modem URC reporting configs...");
                            let _ = at_client.send_command("AT+CNMI=2,1,0,2,0".to_string()).await;
                            let _ = at_client.send_command("AT+CMGF=0".to_string()).await;
                            let _ = at_client.send_command("AT+CLIP=1".to_string()).await;

                            // Detect interface
                            let actual_ifname = detect_modem_ifname(&config.advanced_network_config.ifname).await;
                            info!("Auto-detected 5G interface: {}", actual_ifname);
                            
                            // Setup IPv4 Only
                            if let Err(e) = network::setup_ipv4_only(&config, &actual_ifname).await {
                                error!("Failed to setup IPv4 network: {}", e);
                            } else {
                                state = ConnectionState::IPv4Configured(Instant::now());
                                info!("IPv4 setup done. Waiting for stability before injecting IPv6...");
                            }
                        },
                        ConnectionState::IPv4Configured(start_time) => {
                            // Check config to see if IPv6 is even enabled
                            let pdp_type = config.advanced_network_config.pdp_type.to_lowercase();
                            let ipv6_needed = pdp_type.contains("v6") || pdp_type.contains("ipv6");
                            
                            if !ipv6_needed {
                                state = ConnectionState::FullStackConfigured;
                                continue;
                            }

                            // Wait for 15 seconds
                            if start_time.elapsed().as_secs() >= 15 {
                                // Check Ping
                                if check_ping().await {
                                    info!("IPv4 network is stable (Ping success). Injecting IPv6...");
                                    let actual_ifname = detect_modem_ifname(&config.advanced_network_config.ifname).await;
                                    
                                    if let Err(e) = network::inject_ipv6_interface(&config, &actual_ifname).await {
                                        error!("Failed to inject IPv6 interface: {}", e);
                                    } else {
                                        state = ConnectionState::FullStackConfigured;
                                        info!("IPv6 Injection Completed. Full stack active.");
                                    }
                                } else {
                                    debug!("IPv4 configured but ping failed/unstable. Holding IPv6 injection...");
                                }
                            }
                        },
                        ConnectionState::FullStackConfigured => {
                            // Monitoring stable state
                        }
                    }
                } else {
                    // No IP detected
                    if !matches!(state, ConnectionState::Disconnected) {
                        warn!("Lost IP address. Resetting connection state.");
                        state = ConnectionState::Disconnected;
                    }
                    
                    info!("No IP address detected. Checking auto-dial setting before attempting to dial...");
                    
                    // 1. 先查询模块当前的自动拨号设置
                    let auto_dial_resp = at_client.send_command("AT^SETAUTODIAL?".to_string()).await;
                    
                    let mut should_dial = true; // 默认去拨号
                    
                    if let Ok(resp) = auto_dial_resp {
                        if let Some(data) = resp.data {
                            // 2. 如果返回包含 ^SETAUTODIAL:0，说明用户在网页上手动关闭了拨号
                            if data.contains("^SETAUTODIAL:0") || data.contains("^SETAUTODIAL: 0") {
                                warn!("Auto-dial is DISABLED in modem settings. Rust backend will NOT force dial.");
                                should_dial = false;
                            }
                        }
                    }

                    // 3. 只有在允许拨号的情况下，才执行强行抢救
                    if should_dial {
                        if let Err(e) = perform_dial(&config, &at_client).await {
                            warn!("Dial attempt failed: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to check IP status: {}. Retrying later.", e);
            }
        }

        sleep(Duration::from_secs(10)).await;
    }
}

async fn check_ping() -> bool {
    // Ping Aliyun DNS (223.5.5.5) or 114 DNS
    // -c 1: count 1
    // -W 2: timeout 2 seconds
    let output = Command::new("ping")
        .args(&["-c", "1", "-W", "2", "223.5.5.5"])
        .output()
        .await;
        
    match output {
        Ok(o) => if o.status.success() { return true; } else {
             // Fallback to 114.114.114.114
             let output2 = Command::new("ping")
                .args(&["-c", "1", "-W", "2", "114.114.114.114"])
                .output()
                .await;
             return output2.map(|o| o.status.success()).unwrap_or(false);
        },
        Err(_) => false,
    }
}

async fn check_ip_status(at_client: &ATClient) -> Result<bool> {
    let response = at_client.send_command("AT+CGPADDR".to_string()).await?;
    
    if let Some(content) = response.data {
        debug!("IP Check Response: {}", content);
        
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("+CGPADDR:") {
                // 提取冒号后面的内容，例如: 1,"10.52.0.113","2409:8a00::1"
                let parts: Vec<&str> = line.splitn(2, ':').collect();
                if parts.len() < 2 {
                    continue;
                }
                
                let data_part = parts[1].trim();
                // 按逗号分割，注意可能只有 IPV4，也可能有 IPV6
                let segments: Vec<&str> = data_part.split(',').collect();
                
                let mut found_valid_ip = false;
                
                // segments[0] 通常是 PDP 索引，跳过。从 segments[1] 开始是 IP
                for (_i, segment) in segments.iter().enumerate().skip(1) {
                    // 去除可能存在的引号和多余空格
                    let clean_ip = segment.trim_matches(|c| c == '"' || c == ' ' || c == '\r' || c == '\n');
                    
                    // 过滤掉无效 IP 和空字符串
                    if clean_ip.is_empty() || clean_ip == "0.0.0.0" || clean_ip == "::" {
                        continue;
                    }
                    
                    // 严谨校验：如果是 IPv4，它必须包含点（.）且不能太长
                    if clean_ip.contains('.') && clean_ip.len() <= 15 {
                        info!("Detected IPv4: {}", clean_ip);
                        found_valid_ip = true;
                    }
                    // 严谨校验：如果是 IPv6，它必须包含冒号（:）
                    else if clean_ip.contains(':') && clean_ip.len() <= 39 {
                        info!("Detected IPv6: {}", clean_ip);
                        found_valid_ip = true;
                    }
                }
                
                if found_valid_ip {
                    return Ok(true);
                }
            }
        }
    }
    
    Ok(false)
}

async fn perform_dial(_config: &Config, at_client: &ATClient) -> Result<()> {
    let mut data_profile_id = 1; // 默认上网通道
    let mut ims_profile_id: Option<u32> = None; // 动态寻找的 IMS 通道

    // 1. 查询自动拨号配置，获取前端设定的【上网业务 CID】
    let resp_dial = at_client.send_command("AT^SETAUTODIAL?".to_string()).await;
    if let Ok(response) = resp_dial {
        if let Some(content) = response.data {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with("^SETAUTODIAL:") {
                    let parts: Vec<&str> = line.trim_start_matches("^SETAUTODIAL:").split(',').collect();
                    if parts.len() >= 2 {
                        let enable_flag = parts[0].trim();
                        // 拦截：如果前端关闭了拨号，直接中断
                        if enable_flag == "0" {
                            info!("Auto-dial is disabled in hardware. Aborting perform_dial.");
                            return Ok(());
                        }
                        // 提取前端设置的上网 PDP Profile ID
                        if let Ok(id) = parts[1].trim().parse::<u32>() {
                            data_profile_id = id;
                        }
                    }
                }
            }
        }
    }

    // 2. 扫描所有配置文件，动态揪出【IMS 业务 CID】
    let resp_pdp = at_client.send_command("AT+CGDCONT?".to_string()).await;
    if let Ok(response) = resp_pdp {
        if let Some(content) = response.data {
            for line in content.lines() {
                let line = line.trim();
                // 只要这行的 APN 包含 "ims" (忽略大小写)
                if line.starts_with("+CGDCONT:") && line.to_lowercase().contains("\"ims\"") {
                    let parts: Vec<&str> = line.trim_start_matches("+CGDCONT:").split(',').collect();
                    if let Some(id_str) = parts.first() {
                        if let Ok(id) = id_str.trim().parse::<u32>() {
                            ims_profile_id = Some(id);
                            break; // 找到了就跳出
                        }
                    }
                }
            }
        }
    }

    // 3. 激活【上网业务通道】
    info!("Activating Data Profile ID: {}", data_profile_id);
    let qnet_cmd = format!("AT+QNETDEVCTL={},1,1", data_profile_id);
    let _ = at_client.send_command(qnet_cmd).await;
    
    let cgact_data_cmd = format!("AT+CGACT={},1", data_profile_id);
    let _ = at_client.send_command(cgact_data_cmd).await;

    // (应对某些老固件喜欢用 0 号通道的祖传 Bug)
    if data_profile_id == 1 {
        let _ = at_client.send_command("AT+CGACT=1,0".to_string()).await;
    }

    // 4. 激活【IMS 业务通道】(保障短信和 VoLTE)
    if let Some(ims_id) = ims_profile_id {
        info!("Activating IMS Profile ID: {}", ims_id);
        let cgact_ims_cmd = format!("AT+CGACT={},1", ims_id);
        let _ = at_client.send_command(cgact_ims_cmd).await;
    } else {
        warn!("No IMS profile found! SMS and VoLTE might not work.");
    }

    Ok(())
}

async fn detect_modem_ifname(configured: &str) -> String {
    if !configured.is_empty() && configured != "auto" {
        return configured.to_string();
    }

    if let Some(iface) = detect_modem_interface().await {
        return iface;
    }
    
    // Fallback to a typical USB modem interface, NEVER eth0/eth1
    "usb0".to_string()
}

/// 基于 QModem 原理的绝对精准探测法：直接读取 USB 设备的 Vendor ID (厂商代码)
async fn detect_modem_interface() -> Option<String> {
    let net_dir = "/sys/class/net";
    let Ok(mut entries) = fs::read_dir(net_dir).await else { return None; };

    // 5G/4G 模组的主流厂商 VID 列表 (提取自 QModem 数据库)
    // 3466: Huawei MT5700
    // 2c7c: Quectel
    // 2cb7: Fibocom
    // 12d1: Huawei
    // 19d2: ZTE
    // 05c6: Qualcomm Generic
    let valid_vids = [
        "3466", "2c7c", "2cb7", "12d1", "19d2", "05c6"
    ];

    while let Ok(Some(entry)) = entries.next_entry().await {
        let iface = entry.file_name().into_string().unwrap_or_default();
        // 初级过滤：排除系统内部回环、网桥和无线虚拟网卡
        if iface == "lo" || iface.starts_with("br-") || iface.starts_with("wl") || iface.starts_with("ra") {
            continue;
        }

        // 核心逻辑：读取该网卡对应的物理设备 Vendor ID
        // 路径通常为 /sys/class/net/<iface>/device/idVendor 或 /sys/class/net/<iface>/device/../idVendor
        // 注意：在 tokio fs 中 read_to_string 是异步的
        let vendor_path_direct = format!("{}/{}/device/idVendor", net_dir, iface);
        let vendor_path_parent = format!("{}/{}/device/../idVendor", net_dir, iface);
        
        let mut vid = match fs::read_to_string(&vendor_path_direct).await {
            Ok(v) => v,
            Err(_) => "".to_string()
        };

        if vid.trim().is_empty() {
             match fs::read_to_string(&vendor_path_parent).await {
                Ok(v) => vid = v,
                Err(_) => {}
             }
        }
        
        let vid = vid.trim().to_lowercase();
        if !vid.is_empty() {
            // 如果读取到的厂商 ID 在我们的 5G 模块白名单中
            if valid_vids.contains(&vid.as_str()) {
                info!(
                    "Hardware probing success! Found 5G modem: {} (Vendor ID: {})",
                    iface, vid
                );
                return Some(iface);
            }
        }
    }

    warn!("No valid 5G/4G USB modem interface found based on Vendor ID.");
    None
}

use crate::client::ATClient;
use crate::config::Config;
use crate::network;
use log::{info, warn, error, debug};
use std::time::Duration;
use tokio::time::sleep;
use anyhow::Result;

use tokio::fs;

pub async fn start_monitor(config: Config, at_client: ATClient) {
    info!("Starting dial monitor...");
    
    // Track connection state to avoid repeated setup
    let mut is_connected = false;

    loop {
        // Check IP status
        match check_ip_status(&at_client).await {
            Ok(has_ip) => {
                if has_ip {
                    if !is_connected {
                        info!("IP address detected. Marking as connected.");
                        is_connected = true;
                        
                        // Detect interface
                        let actual_ifname = detect_modem_ifname(&config.advanced_network_config.ifname).await;
                        info!("Auto-detected 5G interface: {}", actual_ifname);
                        
                        // Execute network setup script
                        if let Err(e) = network::setup_modem_network(&config, &actual_ifname).await {
                            error!("Failed to setup modem network: {}", e);
                            // We don't set is_connected to false here, to avoid retrying setup immediately 
                            // unless we lose IP. But maybe we should? 
                            // For now, assume setup failure might be transient or partial, 
                            // and we will retry only if we lose IP and regain it, or we could retry logic.
                            // But requirements say "If ... new connection state ... call setup".
                        }
                    }
                } else {
                    if is_connected {
                        warn!("Lost IP address. Marking as disconnected.");
                        is_connected = false;
                    }
                    
                    info!("No IP address detected. Attempting to dial...");
                    if let Err(e) = perform_dial(&config, &at_client).await {
                        warn!("Dial attempt failed: {}", e);
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

async fn check_ip_status(at_client: &ATClient) -> Result<bool> {
    // Query all PDP contexts to support modules that use index 0, 1, or 3
    let response = at_client.send_command("AT+CGPADDR".to_string()).await?;
    
    // Response format example: +CGPADDR: 1,"10.11.12.13" or +CGPADDR: 1,""
    if let Some(content) = response.data {
        debug!("IP Check Response: {}", content);
        // Simple check: look for a digit or non-empty string inside quotes that isn't 0.0.0.0
        // But some modems return 0.0.0.0 when not connected.
        // And we need to ignore empty strings "".
        
        // Split by lines in case of multiple lines
        for line in content.lines() {
            if line.contains("+CGPADDR:") {
                // Check if we have a valid IP. 
                // A quick heuristic: check if there's a number.
                // Or better, parse the IPs.
                let parts: Vec<&str> = line.split(',').collect();
                if parts.len() >= 2 {
                    // parts[1] should be the first IP
                    let ip1 = parts[1].trim().trim_matches('"');
                    if !ip1.is_empty() && ip1 != "0.0.0.0" {
                        info!("Detected IP: {}", ip1);
                        return Ok(true);
                    }
                    
                    // Check second IP if exists (IPv6)
                    if parts.len() >= 3 {
                        let ip2 = parts[2].trim().trim_matches('"');
                        if !ip2.is_empty() && ip2 != "0.0.0.0" && ip2 != "::" {
                            info!("Detected IPv6: {}", ip2);
                            return Ok(true);
                        }
                    }
                }
            }
        }
    }
    
    Ok(false)
}

async fn perform_dial(config: &Config, at_client: &ATClient) -> Result<()> {
    // 1. Set APN
    // Format: AT+CGDCONT=1,"IP_TYPE","APN"
    // We assume "auto" for APN as per instructions, or maybe empty? 
    // Instructions said: AT+CGDCONT=1,"IPV4V6","auto"
    let pdp_type = config.advanced_network_config.pdp_type.to_uppercase();
    // Ensure pdp_type is valid for AT command (IP, IPV6, IPV4V6)
    let at_pdp_type = if pdp_type.contains("IPV4V6") {
        "IPV4V6"
    } else if pdp_type.contains("IPV6") {
        "IPV6"
    } else {
        "IP" // Default to IP (IPv4)
    };
    
    let apn_cmd = format!("AT+CGDCONT=1,\"{}\",\"auto\"", at_pdp_type);
    let _ = at_client.send_command(apn_cmd).await;
    let _ = at_client.send_command("AT+QNETDEVCTL=1,1,1".to_string()).await;
    let _ = at_client.send_command("AT+CGACT=1,1".to_string()).await;
    // Fallback for Fibocom FM350 and others using PDP 0
    let _ = at_client.send_command("AT+CGACT=1,0".to_string()).await;
    
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

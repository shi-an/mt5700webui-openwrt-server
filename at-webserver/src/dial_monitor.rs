use crate::client::ATClient;
use crate::config::Config;
use crate::network;
use log::{info, warn, error, debug};
use std::time::Duration;
use tokio::time::sleep;
use anyhow::Result;

pub async fn start_monitor(config: Config, at_client: ATClient) {
    info!("Starting dial monitor...");
    
    // Track connection state to avoid repeated setup
    let mut is_connected = false;
    // Default interface name, can be made configurable later
    const IFNAME: &str = "eth1"; 

    loop {
        // Check IP status
        match check_ip_status(&at_client).await {
            Ok(has_ip) => {
                if has_ip {
                    if !is_connected {
                        info!("IP address detected. Marking as connected.");
                        is_connected = true;
                        
                        // Execute network setup script
                        if let Err(e) = network::setup_modem_network(&config, IFNAME).await {
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
    let response = at_client.send_command("AT+CGPADDR=1".to_string()).await?;
    
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
    at_client.send_command(apn_cmd).await?;
    
    // 2. Dial
    // Try QNETDEVCTL (Quectel)
    // AT+QNETDEVCTL=1,1,1 (Context 1, Enable 1, Protocol 1?) - Protocol 1 is usually ECM/QMI? 
    // Actually the command usually is AT+QNETDEVCTL=1,1,1
    // We'll try it.
    let _ = at_client.send_command("AT+QNETDEVCTL=1,1,1".to_string()).await;
    
    // Try CGACT (Standard)
    // AT+CGACT=1,1
    let _ = at_client.send_command("AT+CGACT=1,1".to_string()).await;
    
    Ok(())
}

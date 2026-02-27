use crate::config::Config;
use anyhow::Result;
use log::{error, info};
use tokio::process::Command;

pub async fn setup_modem_network(config: &Config, ifname: &str) -> Result<()> {
    info!("Configuring modem network for interface: {}", ifname);
    
    let net_config = &config.advanced_network_config;
    let pdp_type = &net_config.pdp_type;
    
    // IPv4 Setup
    if pdp_type.contains("ipv4") {
        info!("Setting up IPv4 network...");
        run_uci(&["set", &format!("network.wan_modem=interface")]).await?;
        run_uci(&["set", "network.wan_modem.proto=dhcp"]).await?;
        run_uci(&["set", &format!("network.wan_modem.device={}", ifname)]).await?;
        run_uci(&["set", "network.wan_modem.metric=10"]).await?;
        
        if !net_config.do_not_add_dns && !net_config.dns_list.is_empty() {
            run_uci(&["set", "network.wan_modem.peerdns=0"]).await?;
            // Clear existing dns list first to avoid duplicates or old entries
            run_uci(&["delete", "network.wan_modem.dns"]).await.ok(); 
            for dns in &net_config.dns_list {
                run_uci(&["add_list", &format!("network.wan_modem.dns={}", dns)]).await?;
            }
        } else {
            run_uci(&["set", "network.wan_modem.peerdns=1"]).await?;
        }
    } else {
        // If not using ipv4, maybe we should delete the interface? 
        // For now, following requirements, we just configure if enabled.
        // But QModem logic might imply cleaning up if unused. 
        // Assuming we overwrite or user handles cleanup if switching modes completely.
    }

    // IPv6 Setup
    if pdp_type.contains("ipv6") {
        info!("Setting up IPv6 network...");
        run_uci(&["set", &format!("network.wan_modem6=interface")]).await?;
        run_uci(&["set", "network.wan_modem6.proto=dhcpv6"]).await?;
        run_uci(&["set", &format!("network.wan_modem6.device={}", ifname)]).await?;
        run_uci(&["set", "network.wan_modem6.reqprefix=no"]).await?; 
        
        if net_config.extend_prefix {
            run_uci(&["set", "network.wan_modem6.extendprefix=1"]).await?;
        } else {
            run_uci(&["set", "network.wan_modem6.extendprefix=0"]).await?;
        }

        if net_config.ra_master {
            // Configure DHCP relay/master for IPv6
            run_uci(&["set", "dhcp.wan_modem6=dhcp"]).await?;
            run_uci(&["set", "dhcp.wan_modem6.interface=wan_modem6"]).await?;
            run_uci(&["set", "dhcp.wan_modem6.ra=relay"]).await?;
            run_uci(&["set", "dhcp.wan_modem6.dhcpv6=relay"]).await?;
            run_uci(&["set", "dhcp.wan_modem6.ndp=relay"]).await?;
            run_uci(&["set", "dhcp.wan_modem6.master=1"]).await?;
        } else {
             // Clean up if not master? Or just leave default
             // run_uci(&["delete", "dhcp.wan_modem6"]).await.ok();
        }
    }

    // Apply changes
    info!("Applying network changes...");
    run_uci(&["commit", "network"]).await?;
    run_uci(&["commit", "dhcp"]).await?;
    
    // Restart services
    run_command("/etc/init.d/network", &["restart"]).await?;
    run_command("/etc/init.d/dhcp", &["restart"]).await?;
    
    info!("Network configuration completed.");
    Ok(())
}

async fn run_uci(args: &[&str]) -> Result<()> {
    run_command("uci", args).await
}

async fn run_command(program: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| {
            error!("Failed to execute {}: {}", program, e);
            e
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("Command {} {:?} failed: {}", program, args, stderr);
        // We log error but don't bail immediately to allow partial config application if possible,
        // or we can bail. The requirement says "don't panic, log error".
        // Returning Err here allows caller to handle it, but for setup_modem_network flow
        // we might want to continue best effort or fail fast.
        // Given "log::error!", returning Err is compatible with Result return type.
        return Err(anyhow::anyhow!("Command failed"));
    }
    Ok(())
}

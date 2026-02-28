use crate::config::Config;
use anyhow::Result;
use log::{error, info};
use tokio::process::Command;

pub async fn setup_modem_network(config: &Config, ifname: &str) -> Result<()> {
    info!("Configuring modem network for interface: {}", ifname);
    let net_config = &config.advanced_network_config;
    let pdp_type = &net_config.pdp_type;

    // Generate DNS configuration script based on list
    let mut dns_script = String::new();
    if !net_config.dns_list.is_empty() {
        // User provided DNS list, force use custom DNS
        dns_script.push_str("uci set network.wan_modem.peerdns=0\n");
        dns_script.push_str("uci -q delete network.wan_modem.dns\n");
        for dns in &net_config.dns_list {
            dns_script.push_str(&format!("uci add_list network.wan_modem.dns='{}'\n", dns));
        }
    } else {
        // Empty list, use carrier DNS
        dns_script.push_str("uci set network.wan_modem.peerdns=1\n");
        dns_script.push_str("uci -q delete network.wan_modem.dns\n");
    }
    
    let uci_script = format!(r#"
        # 1. 清理旧配置
        uci -q delete network.wan_modem
        uci -q delete network.wan_modem6
        
        # 2. 配置 IPv4 接口 (主接口，绑定真实物理网卡)
        uci set network.wan_modem=interface
        uci set network.wan_modem.proto='dhcp'
        uci set network.wan_modem.device='{}'
        uci set network.wan_modem.metric='10'
        {}
        
        # 3. 配置 IPv6 接口 (作为 IPv4 的别名，完美复刻 QModem)
        uci set network.wan_modem6=interface
        uci set network.wan_modem6.proto='dhcpv6'
        uci set network.wan_modem6.device='@wan_modem'
        uci set network.wan_modem6.reqaddress='try'
        uci set network.wan_modem6.reqprefix='auto'
        
        # 4. 提交配置落盘
        uci commit network
    "#, ifname, dns_script);

    info!("Executing UCI script for network setup...");
    let output = Command::new("sh")
        .arg("-c")
        .arg(&uci_script)
        .output()
        .await
        .map_err(|e| {
            error!("Failed to execute UCI script: {}", e);
            e
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("UCI script failed: {}", stderr);
        return Err(anyhow::anyhow!("UCI script failed: {}", stderr));
    }

    info!("Binding modem interfaces to firewall wan zone...");
    let fw_script = r#"
        WAN_ZONE=$(uci show firewall | grep "\.name='wan'" | cut -d'.' -f2 | head -n 1)
        if [ -n "$WAN_ZONE" ]; then
            uci del_list firewall.$WAN_ZONE.network='wan_modem' 2>/dev/null
            uci del_list firewall.$WAN_ZONE.network='wan_modem6' 2>/dev/null
            uci add_list firewall.$WAN_ZONE.network='wan_modem'
            uci add_list firewall.$WAN_ZONE.network='wan_modem6'
            uci commit firewall
        fi
        exit 0
    "#;
    let _ = run_command("sh", &["-c", fw_script]).await;

    info!("Bringing up interfaces and reloading firewall...");
    let _ = run_command("ifup", &["wan_modem"]).await;
    if pdp_type.contains("ipv6") {
        let _ = run_command("ifup", &["wan_modem6"]).await;
    }
    
    // 统一由 OpenWrt 的 fw4 / firewall 接管重载
    let _ = run_command("fw4", &["reload"]).await;
    let _ = run_command("/etc/init.d/firewall", &["reload"]).await;
    
    info!("Network configuration completed.");
    Ok(())
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
        return Err(anyhow::anyhow!("Command failed"));
    }
    Ok(())
}

pub async fn teardown_modem_network() -> Result<()> {
    info!("Tearing down modem network by frontend request...");
    // 1. 断开网口
    let _ = run_command("ifdown", &["wan_modem"]).await;
    let _ = run_command("ifdown", &["wan_modem6"]).await;

    // 2. 清理 OpenWrt 配置与防火墙
    let teardown_script = r#"
        uci -q delete network.wan_modem
        uci -q delete network.wan_modem6
        uci commit network
        WAN_ZONE=$(uci show firewall | grep "\.name='wan'" | cut -d'.' -f2 | head -n 1)
        if [ -n "$WAN_ZONE" ]; then
            uci del_list firewall.$WAN_ZONE.network='wan_modem' 2>/dev/null
            uci del_list firewall.$WAN_ZONE.network='wan_modem6' 2>/dev/null
            uci commit firewall
            fw4 reload 2>/dev/null || /etc/init.d/firewall reload 2>/dev/null
        fi
        exit 0
    "#;
    let _ = run_command("sh", &["-c", teardown_script]).await;
    info!("Network interfaces and firewall rules cleared.");
    Ok(())
}


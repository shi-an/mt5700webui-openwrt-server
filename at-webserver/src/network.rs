use crate::config::Config;
use anyhow::Result;
use log::{error, info};
use tokio::process::Command;

pub async fn setup_modem_network(config: &Config, ifname: &str) -> Result<()> {
    info!("Configuring modem network for interface: {}", ifname);
    let net_config = &config.advanced_network_config;
    let pdp_type = &net_config.pdp_type;
    
    // 使用 uci batch 批量构建命令，极大地减少系统 fork 进程的开销
    let mut uci_batch = String::new();
    uci_batch.push_str("delete network.wan_modem\n");
    uci_batch.push_str("delete network.wan_modem6\n");

    if pdp_type.contains("ipv4") {
        uci_batch.push_str("set network.wan_modem=interface\n");
        uci_batch.push_str("set network.wan_modem.proto='dhcp'\n");
        uci_batch.push_str(&format!("set network.wan_modem.ifname='{}'\n", ifname));
        uci_batch.push_str("set network.wan_modem.metric='10'\n");
        
        if !net_config.dns_list.is_empty() {
            uci_batch.push_str("set network.wan_modem.peerdns='0'\n");
            for dns in &net_config.dns_list {
                uci_batch.push_str(&format!("add_list network.wan_modem.dns='{}'\n", dns));
            }
        } else {
            uci_batch.push_str("set network.wan_modem.peerdns='1'\n");
        }
    }

    if pdp_type.contains("ipv6") {
        uci_batch.push_str("set network.wan_modem6=interface\n");
        uci_batch.push_str("set network.wan_modem6.proto='dhcpv6'\n");
        uci_batch.push_str("set network.wan_modem6.ifname='@wan_modem'\n");
        uci_batch.push_str("set network.wan_modem6.metric='10'\n");
        
        // 【核心修复2】：强制要求 IPv6 地址，让 odhcp6c 客户端生成完整状态供 LuCI 读取
        uci_batch.push_str("set network.wan_modem6.reqaddress='force'\n");
        uci_batch.push_str("set network.wan_modem6.reqprefix='auto'\n");
        
        // 【核心修复3】：要求强制将获取到的 5G 前缀扩展委派给内网 (LAN)
        uci_batch.push_str("set network.wan_modem6.extendprefix='1'\n");
        uci_batch.push_str("set network.wan_modem6.defaultroute='1'\n");
        uci_batch.push_str("set network.wan_modem6.peerdns='1'\n");
    }

    uci_batch.push_str("commit network\n");

    // 将批量命令通过单个进程写入
    let script = format!("uci batch <<EOF\n{}EOF", uci_batch);
    if let Err(e) = run_command("sh", &["-c", &script]).await {
        error!("Failed to batch execute UCI configuration: {}", e);
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
    if run_command("fw4", &["reload"]).await.is_err() {
        let _ = run_command("/etc/init.d/firewall", &["reload"]).await;
    }
    
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


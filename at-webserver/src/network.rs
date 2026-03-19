use crate::config::Config;
use anyhow::Result;
use log::{error, info};
use tokio::process::Command;


// 【新增】启动时清理环境，确保无残留配置
pub async fn clean_startup_state() -> Result<()> {
    info!("Performing startup cleanup...");
    let cleanup_script = r#"
        uci -q delete network.wan_modem
        uci -q delete network.wan_modem6
        uci commit network
        
        # 尝试清理可能存在的防火墙残留
        WAN_ZONE=$(uci show firewall | grep "\.name='wan'" | cut -d'.' -f2 | head -n 1)
        if [ -n "$WAN_ZONE" ]; then
            uci del_list firewall.$WAN_ZONE.network='wan_modem' 2>/dev/null
            uci del_list firewall.$WAN_ZONE.network='wan_modem6' 2>/dev/null
            uci commit firewall
        fi
        
        # 重载配置
        /etc/init.d/network reload
        fw4 reload 2>/dev/null || /etc/init.d/firewall reload 2>/dev/null
        exit 0
    "#;
    let _ = run_command("sh", &["-c", cleanup_script]).await;
    info!("Startup cleanup completed.");
    Ok(())
}

pub async fn setup_ipv4_only(config: &Config, ifname: &str) -> Result<()> {
    info!("Setting up IPv4 ONLY for interface: {}", ifname);
    let net_config = &config.advanced_network_config;
    
    // 1. 配置物理设备 dev_xxx 和 wan_modem (IPv4)
    let mut uci_batch = String::new();
    
    // 清理旧配置
    uci_batch.push_str(&format!("delete network.dev_{}\n", ifname));
    uci_batch.push_str("delete network.wan_modem\n");
    
    // 配置物理设备
    uci_batch.push_str(&format!("set network.dev_{}=device\n", ifname));
    uci_batch.push_str(&format!("set network.dev_{}.name='{}'\n", ifname, ifname));
    
    // 配置 IPv4 接口
    uci_batch.push_str("set network.wan_modem=interface\n");
    uci_batch.push_str("set network.wan_modem.proto='dhcp'\n");
    uci_batch.push_str(&format!("set network.wan_modem.device='{}'\n", ifname));
    uci_batch.push_str(&format!("set network.wan_modem.ifname='{}'\n", ifname));
    uci_batch.push_str("set network.wan_modem.metric='10'\n");
    
    // 允许接收 IPv6 RA (即便只配 IPv4，物理层也要允许)
    uci_batch.push_str("set network.wan_modem.ipv6='1'\n");
    
    uci_batch.push_str("set network.wan_modem.delegate='0'\n");
    uci_batch.push_str("set network.wan_modem.auto='1'\n");
    uci_batch.push_str("set network.wan_modem.force_link='1'\n");
    
    if !net_config.dns_list.is_empty() {
        uci_batch.push_str("set network.wan_modem.peerdns='0'\n");
        for dns in &net_config.dns_list {
            uci_batch.push_str(&format!("add_list network.wan_modem.dns='{}'\n", dns));
        }
    } else {
        uci_batch.push_str("set network.wan_modem.peerdns='1'\n");
    }
    
    uci_batch.push_str("commit network\n");
    
    // 执行 UCI 配置
    let script = format!("uci batch <<EOF\n{}EOF", uci_batch);
    if let Err(e) = run_command("sh", &["-c", &script]).await {
        error!("Failed to setup IPv4 UCI: {}", e);
        return Err(e);
    }
    
    // 2. 绑定防火墙 wan_modem
    let fw_script = r#"
        WAN_ZONE=$(uci show firewall | grep "\.name='wan'" | cut -d'.' -f2 | head -n 1)
        if [ -n "$WAN_ZONE" ]; then
            uci del_list firewall.$WAN_ZONE.network='wan_modem' 2>/dev/null
            uci add_list firewall.$WAN_ZONE.network='wan_modem'
            uci commit firewall
        fi
        exit 0
    "#;
    let _ = run_command("sh", &["-c", fw_script]).await;
    
    // 3. 拉起接口
    info!("Bringing up IPv4 interface...");
    let _ = run_command("ifup", &["wan_modem"]).await;
    
    // 4. 重载防火墙
    if run_command("fw4", &["reload"]).await.is_err() {
        let _ = run_command("/etc/init.d/firewall", &["reload"]).await;
    }
    
    info!("IPv4 network setup completed.");
    Ok(())
}

/// 为 MT5700M-CN 配置 IPv6，使用 DHCPv6-PD + RA Relay 模式。
/// 
/// 工作原理：
///   wan_modem6（master=1）从模组通过 DHCPv6-PD 获取前缀并接收 RA，
///   odhcpd 将 RA 和 DHCPv6 中继转发给 LAN，内网设备通过 SLAAC 自动获得真实 IPv6 地址。
///   相比 extendprefix 模式，此方案在运营商仅下发 /64 时同样可靠。
pub async fn inject_ipv6_interface(_config: &Config, ifname: &str) -> Result<()> {
    info!("Injecting IPv6 interface (RA Relay mode) for: {}", ifname);

    // 1. 配置 wan_modem6：DHCPv6 客户端，作为 RA relay master
    let uci_batch = format!(
        "delete network.wan_modem6\n\
         set network.wan_modem6=interface\n\
         set network.wan_modem6.proto='dhcpv6'\n\
         set network.wan_modem6.device='{ifname}'\n\
         set network.wan_modem6.ifname='{ifname}'\n\
         set network.wan_modem6.metric='10'\n\
         set network.wan_modem6.reqaddress='try'\n\
         set network.wan_modem6.reqprefix='auto'\n\
         set network.wan_modem6.norelease='1'\n\
         set network.wan_modem6.auto='1'\n\
         set network.wan_modem6.defaultroute='1'\n\
         set network.wan_modem6.peerdns='1'\n\
         commit network\n",
        ifname = ifname
    );

    let script = format!("uci batch <<EOF\n{}EOF", uci_batch);
    if let Err(e) = run_command("sh", &["-c", &script]).await {
        error!("Failed to setup wan_modem6 UCI: {}", e);
        return Err(e);
    }

    // 2. 配置 odhcpd RA Relay：wan_modem6 作为 master，lan 作为 relay 下游
    //    master=1 表示此接口是上行（从模组接收 RA/前缀），lan 侧转发给内网设备
    let relay_script = r#"
        uci batch <<EOF
set dhcp.wan_modem6=dhcp
set dhcp.wan_modem6.interface='wan_modem6'
set dhcp.wan_modem6.ignore='1'
set dhcp.wan_modem6.ra='relay'
set dhcp.wan_modem6.ndp='relay'
set dhcp.wan_modem6.master='1'
set dhcp.lan.ra='relay'
set dhcp.lan.ndp='relay'
set dhcp.lan.dhcpv6='relay'
commit dhcp
EOF
    "#;
    if let Err(e) = run_command("sh", &["-c", relay_script]).await {
        error!("Failed to setup odhcpd relay UCI: {}", e);
        return Err(e);
    }

    // 3. 绑定防火墙 wan zone
    let fw_script = r#"
        WAN_ZONE=$(uci show firewall | grep "\.name='wan'" | cut -d'.' -f2 | head -n 1)
        if [ -n "$WAN_ZONE" ]; then
            uci del_list firewall.$WAN_ZONE.network='wan_modem6' 2>/dev/null
            uci add_list firewall.$WAN_ZONE.network='wan_modem6'
            uci commit firewall
        fi
        exit 0
    "#;
    let _ = run_command("sh", &["-c", fw_script]).await;

    // 4. 拉起接口并重启 odhcpd
    info!("Bringing up IPv6 interface and restarting odhcpd...");
    let _ = run_command("ifup", &["wan_modem6"]).await;
    // 重启 odhcpd 使 relay 配置生效
    let _ = run_command("/etc/init.d/odhcpd", &["restart"]).await;

    // 5. 重载防火墙
    if run_command("fw4", &["reload"]).await.is_err() {
        let _ = run_command("/etc/init.d/firewall", &["reload"]).await;
    }

    info!("IPv6 RA Relay injection completed.");
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


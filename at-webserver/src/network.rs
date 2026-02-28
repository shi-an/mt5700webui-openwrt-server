use crate::config::Config;
use anyhow::Result;
use log::{error, info};
use tokio::process::Command;

pub async fn setup_modem_network(_config: &Config, ifname: &str) -> Result<()> {
    info!("Configuring modem network for interface: {}", ifname);
    
    let uci_script = format!(r#"
        # 1. 清理旧配置
        uci -q delete network.wan_modem
        uci -q delete network.wan_modem6
        
        # 2. 配置 IPv4 接口 (主接口，绑定真实物理网卡)
        uci set network.wan_modem=interface
        uci set network.wan_modem.proto='dhcp'
        uci set network.wan_modem.device='{}'
        uci set network.wan_modem.metric='10'
        
        # 3. 配置 IPv6 接口 (作为 IPv4 的别名，完美复刻 QModem)
        uci set network.wan_modem6=interface
        uci set network.wan_modem6.proto='dhcpv6'
        uci set network.wan_modem6.device='@wan_modem'
        uci set network.wan_modem6.reqaddress='try'
        uci set network.wan_modem6.reqprefix='auto'
        
        # 4. 提交配置落盘
        uci commit network
        
        # 5. 精确拉起 5G 接口 (不影响本地 LAN/WiFi)
        ifup wan_modem
        ifup wan_modem6
    "#, ifname);

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

    // 精准绑定到防火墙 wan 区域并重载
    let fw_script = r#"
        WAN_ZONE=$(uci show firewall | grep "=zone" | grep -B 1 "name='wan'" | cut -d'.' -f2 | head -n 1)
        if [ -n "$WAN_ZONE" ]; then
            uci del_list firewall.$WAN_ZONE.network='wan_modem' 2>/dev/null
            uci del_list firewall.$WAN_ZONE.network='wan_modem6' 2>/dev/null
            uci add_list firewall.$WAN_ZONE.network='wan_modem'
            uci add_list firewall.$WAN_ZONE.network='wan_modem6'
            uci commit firewall
            /etc/init.d/firewall reload 2>/dev/null || fw4 reload 2>/dev/null
        fi
        exit 0
    "#;
    let _ = run_command("sh", &["-c", fw_script]).await;
    
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


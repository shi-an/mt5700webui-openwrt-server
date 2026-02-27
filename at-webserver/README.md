# AT WebServer 软件包

这个软件包为 OpenWrt 提供了一个 WebSocket AT 命令服务器和 Web 界面，支持高级网络管理、短信收发、定时任务和多通道通知推送。

## 🚀 主要功能

- **AT 命令管理**：通过 WebSocket 实时发送和接收 AT 命令。
- **高级网络配置**：
  - 支持 IPv4/IPv6 双栈拨号 (PDP Type)。
  - 支持 IPv6 RA Master 和前缀扩展。
  - 智能网卡自动探测 (Auto Detect)。
- **定时锁频 (Band Locking)**：
  - 支持日间/夜间双模式定时切换。
  - 支持锁定频段 (Band)、频点 (EARFCN/NR-ARFCN)、PCI 和 SCS。
  - 自动飞行模式切换以生效配置。
- **多通道通知推送**：
  - 支持 PushPlus, Server酱, PushDeer, 飞书, 钉钉, Bark, Telegram, Webhook, 自定义脚本等 10 种通道。
  - 支持短信、来电、内存满、信号变动通知。
- **系统日志监控**：
  - 通过 WebSocket 实时推送系统日志到 Web 前端。

## 📁 文件结构

```
/usr/bin/at-webserver           # Rust 编译的主程序
/etc/init.d/at-webserver        # 系统服务脚本
/etc/config/at-webserver        # UCI 配置文件
```

## 🔧 UCI 配置说明

配置文件路径：`/etc/config/at-webserver`

```bash
config at-webserver 'config'
    option enabled '1'
    
    # 连接配置
    option connection_type 'NETWORK'
    option network_host '192.168.8.1'
    option network_port '20249'
    
    # 高级网络配置
    option pdp_type 'ipv4v6'             # ipv4, ipv6, ipv4v6
    option ifname 'auto'                 # auto 或具体接口名 (如 eth1)
    option ra_master '0'
    
    # 定时锁频配置
    option schedule_enabled '1'
    option schedule_night_enabled '1'
    option schedule_night_start '22:00'
    option schedule_night_end '06:00'
    option schedule_night_lte_type '3'   # 3=频段锁定
    option schedule_night_lte_bands '3,8'
    
    # 通知配置 (多选)
    option enabled_push_services 'wechat telegram bark'
    option wechat_webhook 'https://qyapi.weixin.qq.com/...'
    option tg_bot_token '123456:ABC...'
    option tg_chat_id '123456'
    option bark_url 'https://api.day.app/KEY/'
```

## 📦 依赖包

- libc
- libgcc
- libpthread

## 🔨 编译说明

本软件包使用 Rust 编写。在 OpenWrt SDK 中编译时，确保 feeds 中包含 `lang/rust` 支持。

```bash
# 进入 SDK 目录
./scripts/feeds update -a
./scripts/feeds install -a

# 编译
make package/at-webserver/compile
```

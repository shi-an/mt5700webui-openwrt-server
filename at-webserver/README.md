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
    option connection_type 'SERIAL'
    option serial_port 'auto'               # 自动枚举并验证 AT 控制通道
    option network_host '192.168.8.1'
    option network_port '20249'
    
    # 高级网络配置
    option pdp_type 'ipv4v6'             # ipv4, ipv6, ipv4v6
    option ifname 'auto'                 # auto、逻辑接口 (wan2) 或物理口 (eth2)
    option ra_master '0'
    option sms_storage 'SM'              # SM=SIM 卡，ME=模组存储
    
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

`sms_storage` 默认为 `SM`（SIM 卡），也可设为 `ME`（模组存储）。后端会在每次 AT 控制通道建立或重新连接后，通过 `AT+CPMS` 同时设置读取、写入和接收存储；该初始化不依赖拨号 IP 或路由侧链路。

后端会先查询 `AT^SETAUTODIAL?` 的拨号模式，再选择数据网卡：

- 模式 `1`：USB 虚拟网卡，按模组 USB Vendor ID 探测 ECM/NCM 接口。
- 模式 `2`：转网口模式，默认复用 OpenWrt 原生逻辑接口 `wan`，不会在同一物理口上启动第二个 DHCP 客户端。

默认控制连接为 `SERIAL + auto`。后端优先枚举 VID `3466` 下的 `ttyUSB*`/`ttyACM*`，逐个发送 `AT`，仅使用返回完整 `OK` 的控制通道。宽带与模组组成多 WAN 时，应将 `ifname` 设置为模组对应的逻辑接口（如 `wan2`）；填写未配置的物理口（如 `eth2`）时，后端会创建并管理 `wan_modem`。

后端将健康状态分为三层，避免把路由侧 DHCP 故障误判为模组掉线：

1. **工作模式**：定期查询 `AT^SETAUTODIAL?`，区分 USB 虚拟网卡和转网口模式。
2. **模组数据会话**：只跟踪数据 CID `1`。USB 虚拟网卡模式使用 `^NDISSTAT` / `AT^NDISSTATQRY` 判断 NDIS 状态，并用 `AT+CGPADDR` 确认 PDP 地址；转网口模式忽略 NDIS 状态，直接使用 `AT+CGPADDR` 检查模组 PDP 地址。IMS 等其他 CID 不会触发重拨。
3. **路由侧链路**：检查对应 OpenWrt 接口的 carrier、IPv4 地址和默认路由。IPv4 或路由连续异常时只重启该逻辑接口；转网口模式的网线 carrier 断开时只等待链路恢复，不重拨模组。

只有 CID `1` 会话确认断开、持续无地址或连续探测异常，以及 USB 数据接口 carrier 连续异常时，后端才通过 `AT^NDISDUP=1,0/1` 重建数据会话。恢复流程不会执行模组重启、`AT+CFUN` 或 OpenWrt 整机重启。

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

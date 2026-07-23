use crate::client::ATClient;
use crate::config::Config;
use crate::models::{get_modem_session_tx, ModemSessionState};
use crate::network;
use log::{info, warn, error, debug};
use std::time::{Duration, Instant};
use tokio::time::{sleep, interval, MissedTickBehavior};
use tokio::process::Command;
use anyhow::{anyhow, bail, Result};

use tokio::fs;

const DATA_CID: u8 = 1;
const SESSION_QUERY_FAILURE_LIMIT: u8 = 3;

/// IP 连接状态，参考 QModem modem_dial.sh 的 connection_status 四状态设计
#[derive(Debug, Clone, PartialEq)]
enum IpStatus {
    /// AT 命令响应异常（非预期内容）
    Unexpected,
    /// 有响应但无有效 IP
    NoIp,
    /// 仅 IPv4
    Ipv4Only(String),
    /// 仅 IPv6
    Ipv6Only(String),
    /// IPv4 + IPv6 双栈
    DualStack(String, String),
}

impl IpStatus {
    fn has_ip(&self) -> bool {
        !matches!(self, IpStatus::Unexpected | IpStatus::NoIp)
    }
}

enum ConnectionState {
    Disconnected,
    DataPathConfigured,
}

#[derive(Debug, PartialEq)]
enum DataSessionStatus {
    Connected(IpStatus),
    Connecting,
    NoAddress,
    Disconnected,
    Unexpected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionQueryMethod {
    Discover(u8),
    QueryAll,
    QueryCid,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouterLinkStatus {
    Ready,
    DeviceMissing,
    CarrierDown,
    InterfaceUnavailable,
    InterfaceDown,
    MissingIpv4,
    MissingDefaultRoute,
}

impl RouterLinkStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::DeviceMissing => "device missing",
            Self::CarrierDown => "physical carrier down",
            Self::InterfaceUnavailable => "ifstatus unavailable",
            Self::InterfaceDown => "logical interface down",
            Self::MissingIpv4 => "IPv4 address missing",
            Self::MissingDefaultRoute => "default route missing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DataInterface {
    device: String,
    logical_interface: String,
    managed: bool,
}

impl DataInterface {
    fn managed(device: String) -> Self {
        Self {
            device,
            logical_interface: "wan_modem".to_string(),
            managed: true,
        }
    }

    fn existing(logical_interface: &str, device: String) -> Self {
        Self {
            device,
            logical_interface: logical_interface.to_string(),
            managed: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataMode {
    Usb,
    Ethernet,
}

impl DataMode {
    fn label(self) -> &'static str {
        match self {
            Self::Usb => "USB virtual network interface",
            Self::Ethernet => "Ethernet port",
        }
    }
}

pub async fn start_monitor(config: Config, at_client: ATClient) {
    info!("Starting mode-aware modem session and router link monitor...");

    let mut state = ConnectionState::Disconnected;
    let mut router_fail_count = 0u32;
    let mut session_fail_count = 0u32;
    let mut unexpected_response_count = 0u32;
    let mut data_mode: Option<DataMode> = None;
    let mut active_interface: Option<DataInterface> = None;
    let mut session_query_method = SessionQueryMethod::Discover(0);
    let mut last_session_recovery: Option<Instant> = None;

    let session_tx = get_modem_session_tx();
    let mut session_rx = session_tx.subscribe();

    let mut poll_timer = interval(Duration::from_secs(10));
    poll_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        let mut session_event = tokio::select! {
            _ = poll_timer.tick() => None,
            result = session_rx.recv() => {
                match result {
                    Ok(event) => Some(event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("[NDISSTAT] Missed {} modem session events; polling current state.", n);
                        None
                    }
                    Err(_) => None,
                }
            }
        };

        match detect_data_mode(&at_client).await {
            Ok(detected) => {
                if data_mode != Some(detected) {
                    let previous_mode = data_mode;
                    if let Some(previous) = previous_mode {
                        warn!(
                            "Modem data mode changed: {} -> {}.",
                            previous.label(),
                            detected.label()
                        );
                        let _ = network::teardown_modem_network().await;
                    } else {
                        info!("Detected modem data mode: {}.", detected.label());
                    }
                    data_mode = Some(detected);
                    state = ConnectionState::Disconnected;
                    active_interface = None;
                    router_fail_count = 0;
                    session_fail_count = 0;
                    unexpected_response_count = 0;
                    session_query_method = SessionQueryMethod::Discover(0);
                    last_session_recovery = None;
                    if previous_mode.is_some() {
                        session_event = None;
                        drain_modem_session_events(&mut session_rx);
                    }
                }
            }
            Err(e) => {
                warn!("Unable to determine modem data mode: {}", e);
                if data_mode.is_none() {
                    continue;
                }
            }
        }

        let Some(active_mode) = data_mode else {
            continue;
        };

        if let Some(event) = session_event {
            if !is_data_session_cid(event.cid) {
                debug!(
                    "Ignoring modem session event for unrelated cid {:?}; data cid is {}.",
                    event.cid,
                    DATA_CID
                );
            } else {
                match event.state {
                    ModemSessionState::Connected => {
                        debug!(
                            "[NDISSTAT] Modem session connected event received (cid={:?}).",
                            event.cid
                        );
                    }
                    ModemSessionState::Connecting => {
                        info!(
                            "[NDISSTAT] Modem session is connecting (cid={:?}); waiting for completion.",
                            event.cid
                        );
                        state = ConnectionState::Disconnected;
                        active_interface = None;
                        session_fail_count = 0;
                        continue;
                    }
                    ModemSessionState::Disconnected => {
                        if is_auto_dial_disabled(&at_client).await {
                            debug!("[monitor] Modem session disconnected while auto dial is disabled.");
                            state = ConnectionState::Disconnected;
                            active_interface = None;
                            session_fail_count = 0;
                            continue;
                        }

                        if last_session_recovery
                            .map(|last| last.elapsed() < Duration::from_secs(15))
                            .unwrap_or(false)
                        {
                            debug!("Ignoring a delayed disconnect event from the recent recovery; polling current state.");
                        } else {
                            warn!(
                                "[NDISSTAT] Modem session disconnected in {} mode; reconnecting session.",
                                active_mode.label()
                            );
                            let recovered = recover_modem_session(&at_client, active_mode, false).await;
                            session_query_method = SessionQueryMethod::Discover(0);
                            last_session_recovery = Some(Instant::now());
                            drain_modem_session_events(&mut session_rx);
                            if !recovered {
                                warn!("Modem session reconnect failed; retrying on a later poll.");
                            }
                            state = ConnectionState::Disconnected;
                            active_interface = None;
                            router_fail_count = 0;
                            session_fail_count = 0;
                            unexpected_response_count = 0;
                            continue;
                        }
                    }
                }
            }
        }

        match check_data_session(
            &at_client,
            &mut session_query_method,
            active_mode,
        ).await {
            Ok(DataSessionStatus::Connecting) => {
                debug!("Modem data session is still connecting.");
                state = ConnectionState::Disconnected;
                active_interface = None;
                session_fail_count = 0;
                continue;
            }
            Ok(DataSessionStatus::Disconnected) => {
                unexpected_response_count = 0;
                router_fail_count = 0;
                state = ConnectionState::Disconnected;
                active_interface = None;

                if is_auto_dial_disabled(&at_client).await {
                    debug!("Modem session is down while auto dial is disabled.");
                    session_fail_count = 0;
                    continue;
                }

                session_fail_count = session_fail_count.saturating_add(1);
                warn!(
                    "Modem session is down in {} mode. Count: {}/3",
                    active_mode.label(),
                    session_fail_count
                );
                if session_fail_count >= 3 {
                    let recovered = recover_modem_session(&at_client, active_mode, true).await;
                    session_query_method = SessionQueryMethod::Discover(0);
                    last_session_recovery = Some(Instant::now());
                    drain_modem_session_events(&mut session_rx);
                    if !recovered {
                        warn!("Forced modem session recovery failed.");
                    }
                    session_fail_count = 0;
                }
            }
            Ok(DataSessionStatus::NoAddress) => {
                unexpected_response_count = 0;
                state = ConnectionState::Disconnected;
                active_interface = None;
                if is_auto_dial_disabled(&at_client).await {
                    debug!("Modem session has no address while auto dial is disabled.");
                    session_fail_count = 0;
                    continue;
                }
                session_fail_count = session_fail_count.saturating_add(1);
                warn!(
                    "Modem session reports connected but has no PDP address. Count: {}/3",
                    session_fail_count
                );
                if session_fail_count >= 3 {
                    let recovered = recover_modem_session(&at_client, active_mode, true).await;
                    session_query_method = SessionQueryMethod::Discover(0);
                    last_session_recovery = Some(Instant::now());
                    drain_modem_session_events(&mut session_rx);
                    if !recovered {
                        warn!("Recovery of the address-less modem session failed.");
                    }
                    session_fail_count = 0;
                }
            }
            Ok(DataSessionStatus::Unexpected) => {
                unexpected_response_count = unexpected_response_count.saturating_add(1);
                warn!(
                    "Modem session probe returned an unexpected response. Count: {}/3",
                    unexpected_response_count
                );
                if unexpected_response_count >= 3 {
                    if is_auto_dial_disabled(&at_client).await {
                        debug!("Skipping probe-failure recovery because auto dial is disabled.");
                        unexpected_response_count = 0;
                        continue;
                    }
                    let recovered = recover_modem_session(&at_client, active_mode, true).await;
                    session_query_method = SessionQueryMethod::Discover(0);
                    last_session_recovery = Some(Instant::now());
                    drain_modem_session_events(&mut session_rx);
                    if !recovered {
                        warn!("Modem session recovery after probe failures did not succeed.");
                    }
                    unexpected_response_count = 0;
                    state = ConnectionState::Disconnected;
                    active_interface = None;
                }
            }
            Ok(DataSessionStatus::Connected(ref ip_status)) => {
                session_fail_count = 0;
                unexpected_response_count = 0;
                log_ip_status(ip_status);

                match state {
                    ConnectionState::Disconnected => {
                        info!("Modem session has an IP address. Preparing router data path...");
                        let selection = match detect_data_interface(
                            &config.advanced_network_config.ifname,
                            active_mode,
                        ).await {
                            Ok(selection) => selection,
                            Err(e) => {
                                error!("Failed to select router data interface: {}", e);
                                active_interface = None;
                                continue;
                            }
                        };
                        info!(
                            "Using data device {} through OpenWrt interface {} for {} mode.",
                            selection.device,
                            selection.logical_interface,
                            active_mode.label(),
                        );

                        active_interface = Some(selection.clone());
                        state = ConnectionState::DataPathConfigured;

                        if device_carrier_down(&selection.device).await {
                            if active_mode == DataMode::Ethernet {
                                warn!(
                                    "Ethernet carrier is down on {} (device {}); waiting without redialing modem.",
                                    selection.logical_interface,
                                    selection.device
                                );
                                if !selection.managed {
                                    router_fail_count = 0;
                                    continue;
                                }
                            } else {
                                warn!(
                                    "USB data-interface carrier is down on {}. Count: 1/3",
                                    selection.device
                                );
                            }
                        }

                        match configure_router_data_path(&config, &at_client, &selection).await {
                            Ok(()) => {
                                info!(
                                    "Router data path is active on {} (device {}, mode {}).",
                                    selection.logical_interface,
                                    selection.device,
                                    active_mode.label()
                                );
                                router_fail_count = 0;
                            }
                            Err(e) => {
                                error!("Failed to prepare router data path: {}", e);
                                router_fail_count = 1;
                            }
                        }
                    }
                    ConnectionState::DataPathConfigured => {
                        let Some(selection) = active_interface.clone() else {
                            state = ConnectionState::Disconnected;
                            continue;
                        };
                        let link_status = check_router_network_status(&selection).await;
                        match link_status {
                            RouterLinkStatus::Ready => {
                                if router_fail_count > 0 {
                                    info!("Router-side data path recovered on {}.", selection.logical_interface);
                                }
                                router_fail_count = 0;
                            }
                            RouterLinkStatus::CarrierDown => {
                                if active_mode == DataMode::Ethernet {
                                    router_fail_count = 0;
                                    warn!(
                                        "Ethernet carrier is down on {} (device {}); waiting without redialing modem.",
                                        selection.logical_interface,
                                        selection.device
                                    );
                                } else {
                                    router_fail_count = router_fail_count.saturating_add(1);
                                    warn!(
                                        "USB data-interface carrier is down on {}. Count: {}/3",
                                        selection.device,
                                        router_fail_count
                                    );
                                    if router_fail_count >= 3 {
                                        if is_auto_dial_disabled(&at_client).await {
                                            debug!("Skipping USB session recovery because auto dial is disabled.");
                                            router_fail_count = 0;
                                            continue;
                                        }
                                        let recovered = recover_modem_session(&at_client, active_mode, true).await;
                                        session_query_method = SessionQueryMethod::Discover(0);
                                        last_session_recovery = Some(Instant::now());
                                        drain_modem_session_events(&mut session_rx);
                                        if !recovered {
                                            warn!("USB data-session recovery failed.");
                                        }
                                        state = ConnectionState::Disconnected;
                                        active_interface = None;
                                        router_fail_count = 0;
                                    }
                                }
                            }
                            RouterLinkStatus::DeviceMissing => {
                                warn!(
                                    "Router data device {} disappeared; waiting for interface detection.",
                                    selection.device
                                );
                                state = ConnectionState::Disconnected;
                                active_interface = None;
                                router_fail_count = 0;
                            }
                            failure => {
                                router_fail_count = router_fail_count.saturating_add(1);
                                warn!(
                                    "Router-side {} on {} (mode {}). Count: {}/3",
                                    failure.label(),
                                    selection.logical_interface,
                                    active_mode.label(),
                                    router_fail_count
                                );
                                if router_fail_count >= 3 {
                                    warn!(
                                        "Restarting OpenWrt interface {} without redialing the modem session.",
                                        selection.logical_interface
                                    );
                                    match network::restart_ipv4_interface(&selection.logical_interface).await {
                                        Ok(()) => {
                                            info!("OpenWrt interface {} recovered.", selection.logical_interface);
                                            router_fail_count = 0;
                                        }
                                        Err(e) => {
                                            warn!("OpenWrt interface recovery failed: {}", e);
                                            state = ConnectionState::Disconnected;
                                            active_interface = None;
                                            router_fail_count = 0;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to probe modem data session: {}", e);
            }
        }
    }
}

async fn configure_router_data_path(
    config: &Config,
    at_client: &ATClient,
    selection: &DataInterface,
) -> Result<()> {
    debug!("Initializing modem URC reporting configs...");
    let _ = at_client.send_command("AT+CNMI=2,1,0,2,0".to_string()).await;
    let _ = at_client.send_command("AT+CMGF=0".to_string()).await;
    let _ = at_client.send_command("AT+CLIP=1".to_string()).await;

    let sms_mem = &config.advanced_network_config.sms_storage;
    let cpms_cmd = format!("AT+CPMS=\"{}\",\"{}\",\"{}\"", sms_mem, sms_mem, sms_mem);
    let _ = at_client.send_command(cpms_cmd).await;

    if selection.managed {
        network::setup_ipv4_only(config, &selection.device).await?;
    } else {
        network::ensure_existing_ipv4_interface(&selection.logical_interface).await?;
    }

    let pdp_type = config.advanced_network_config.pdp_type.to_lowercase();
    let ipv6_needed = pdp_type.contains("v6") || pdp_type.contains("ipv6");
    if ipv6_needed && selection.managed {
        if let Err(e) = network::inject_ipv6_interface(config, &selection.device).await {
            error!("Failed to inject IPv6 interface: {}", e);
        }
    } else if ipv6_needed {
        debug!(
            "Reusing existing OpenWrt interface {}; preserving its IPv6 configuration.",
            selection.logical_interface
        );
    }

    Ok(())
}

/// 打印当前 IP 状态到日志
fn log_ip_status(status: &IpStatus) {
    match status {
        IpStatus::Ipv4Only(v4) => debug!("Connection status: IPv4 only ({})", v4),
        IpStatus::Ipv6Only(v6) => debug!("Connection status: IPv6 only ({})", v6),
        IpStatus::DualStack(v4, v6) => debug!("Connection status: Dual Stack (v4={}, v6={})", v4, v6),
        _ => {}
    }
}

async fn check_router_network_status(selection: &DataInterface) -> RouterLinkStatus {
    let device_path = format!("/sys/class/net/{}", selection.device);
    if fs::metadata(&device_path).await.is_err() {
        return RouterLinkStatus::DeviceMissing;
    }

    if device_carrier_down(&selection.device).await {
        return RouterLinkStatus::CarrierDown;
    }

    let output = match Command::new("ifstatus")
        .arg(&selection.logical_interface)
        .output()
        .await
    {
        Ok(output) if output.status.success() => output,
        _ => return RouterLinkStatus::InterfaceUnavailable,
    };
    let status: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(status) => status,
        Err(_) => return RouterLinkStatus::InterfaceUnavailable,
    };

    if !status.get("up").and_then(|value| value.as_bool()).unwrap_or(false) {
        return RouterLinkStatus::InterfaceDown;
    }
    let has_ipv4 = status
        .get("ipv4-address")
        .and_then(|value| value.as_array())
        .map(|addresses| !addresses.is_empty())
        .unwrap_or(false);
    if !has_ipv4 {
        return RouterLinkStatus::MissingIpv4;
    }
    let has_default_route = status
        .get("route")
        .and_then(|value| value.as_array())
        .map(|routes| {
            routes.iter().any(|route| {
                route
                    .get("target")
                    .and_then(|target| target.as_str())
                    .map(|target| target == "0.0.0.0")
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if !has_default_route {
        return RouterLinkStatus::MissingDefaultRoute;
    }

    RouterLinkStatus::Ready
}

async fn device_carrier_down(device: &str) -> bool {
    fs::read_to_string(format!("/sys/class/net/{}/carrier", device))
        .await
        .map(|carrier| carrier.trim() == "0")
        .unwrap_or(false)
}

async fn check_data_session(
    at_client: &ATClient,
    query_method: &mut SessionQueryMethod,
    data_mode: DataMode,
) -> Result<DataSessionStatus> {
    let queried_state = query_modem_session_state(at_client, query_method).await;
    if let Some(status) = authoritative_session_status(data_mode, queried_state) {
        return Ok(status);
    }

    Ok(resolve_ip_session_status(
        queried_state,
        check_ip_status(at_client).await?,
    ))
}

fn authoritative_session_status(
    data_mode: DataMode,
    queried_state: Option<ModemSessionState>,
) -> Option<DataSessionStatus> {
    if data_mode != DataMode::Usb {
        return None;
    }

    match queried_state {
        Some(ModemSessionState::Disconnected) => Some(DataSessionStatus::Disconnected),
        Some(ModemSessionState::Connecting) => Some(DataSessionStatus::Connecting),
        _ => None,
    }
}

fn resolve_ip_session_status(
    queried_state: Option<ModemSessionState>,
    ip_status: IpStatus,
) -> DataSessionStatus {
    match ip_status {
        status if status.has_ip() => DataSessionStatus::Connected(status),
        IpStatus::NoIp if queried_state == Some(ModemSessionState::Disconnected) => {
            DataSessionStatus::Disconnected
        }
        IpStatus::NoIp if queried_state == Some(ModemSessionState::Connecting) => {
            DataSessionStatus::Connecting
        }
        IpStatus::NoIp if queried_state == Some(ModemSessionState::Connected) => {
            DataSessionStatus::NoAddress
        }
        IpStatus::NoIp => DataSessionStatus::Disconnected,
        IpStatus::Unexpected => DataSessionStatus::Unexpected,
        _ => DataSessionStatus::Unexpected,
    }
}

async fn query_modem_session_state(
    at_client: &ATClient,
    query_method: &mut SessionQueryMethod,
) -> Option<ModemSessionState> {
    match *query_method {
        SessionQueryMethod::Unsupported => None,
        SessionQueryMethod::QueryAll => {
            match try_session_query(at_client, "AT^NDISSTATQRY?").await {
                Some(state) => Some(state),
                None => {
                    warn!("AT^NDISSTATQRY? stopped returning valid data; rediscovering query support.");
                    *query_method = SessionQueryMethod::Discover(1);
                    None
                }
            }
        }
        SessionQueryMethod::QueryCid => {
            let command = format!("AT^NDISSTATQRY={}", DATA_CID);
            match try_session_query(at_client, &command).await {
                Some(state) => Some(state),
                None => {
                    warn!(
                        "AT^NDISSTATQRY={} stopped returning valid data; rediscovering query support.",
                        DATA_CID
                    );
                    *query_method = SessionQueryMethod::Discover(1);
                    None
                }
            }
        }
        SessionQueryMethod::Discover(failure_count) => {
            if let Some(state) = try_session_query(at_client, "AT^NDISSTATQRY?").await {
                info!("Using AT^NDISSTATQRY? for modem data-session polling.");
                *query_method = SessionQueryMethod::QueryAll;
                return Some(state);
            }
            let command = format!("AT^NDISSTATQRY={}", DATA_CID);
            if let Some(state) = try_session_query(at_client, &command).await {
                info!(
                    "Using AT^NDISSTATQRY={} for modem data-session polling.",
                    DATA_CID
                );
                *query_method = SessionQueryMethod::QueryCid;
                return Some(state);
            }

            let failure_count = failure_count.saturating_add(1);
            if failure_count >= SESSION_QUERY_FAILURE_LIMIT {
                info!(
                    "NDISSTATQRY returned no valid state for {} consecutive probes; using CGPADDR as the modem session fallback.",
                    failure_count
                );
                *query_method = SessionQueryMethod::Unsupported;
            } else {
                warn!(
                    "NDISSTATQRY probe failed ({}/{}); retrying discovery on the next poll.",
                    failure_count,
                    SESSION_QUERY_FAILURE_LIMIT
                );
                *query_method = SessionQueryMethod::Discover(failure_count);
            }
            None
        }
    }
}

async fn try_session_query(at_client: &ATClient, command: &str) -> Option<ModemSessionState> {
    let response = at_client.send_command(command.to_string()).await.ok()?;
    if !response.success {
        return None;
    }
    parse_ndis_query_state(response.data.as_deref()?, DATA_CID)
}

fn parse_ndis_query_state(data: &str, target_cid: u8) -> Option<ModemSessionState> {
    let statuses: Vec<u8> = data
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("^NDISSTATQRY:")
                .map(str::trim)
        })
        .flat_map(|payload| parse_ndis_query_payload(payload, target_cid))
        .collect();
    aggregate_ndis_states(&statuses)
}

fn parse_ndis_query_payload(payload: &str, target_cid: u8) -> Vec<u8> {
    let fields: Vec<&str> = payload.split(',').map(str::trim).collect();
    if fields.is_empty() {
        return Vec::new();
    }

    let cid_prefixed = fields.len() >= 5 && fields.len() % 5 == 0;
    if cid_prefixed {
        fields
            .chunks(5)
            .filter_map(|group| {
                let cid = group.first()?.parse::<u8>().ok()?;
                (cid == target_cid)
                    .then(|| group.get(1)?.parse::<u8>().ok())
                    .flatten()
            })
            .collect()
    } else {
        fields
            .chunks(4)
            .filter_map(|group| group.first()?.parse::<u8>().ok())
            .collect()
    }
}

fn aggregate_ndis_states(statuses: &[u8]) -> Option<ModemSessionState> {
    if statuses.is_empty() {
        return None;
    }
    if statuses.iter().any(|status| *status == 1) {
        Some(ModemSessionState::Connected)
    } else if statuses.iter().any(|status| *status == 2) {
        Some(ModemSessionState::Connecting)
    } else if statuses.iter().all(|status| matches!(status, 0 | 3)) {
        Some(ModemSessionState::Disconnected)
    } else {
        None
    }
}

/// 检查 IP 状态，返回精细的四状态枚举
/// 参考 QModem modem_dial.sh check_ip() 的 connection_status 设计
async fn check_ip_status(at_client: &ATClient) -> Result<IpStatus> {
    let response = at_client.send_command("AT+CGPADDR".to_string()).await?;

    let content = match response.data {
        Some(c) => c,
        None => {
            warn!("AT+CGPADDR returned no data.");
            return Ok(IpStatus::Unexpected);
        }
    };

    debug!("IP Check Response: {}", content);

    let status = parse_ip_status(&content, DATA_CID);
    if status == IpStatus::Unexpected {
        warn!(
            "AT+CGPADDR response contains no +CGPADDR line: {}",
            content.replace(['\n', '\r'], " ")
        );
    }
    Ok(status)
}

fn parse_ip_status(content: &str, target_cid: u8) -> IpStatus {
    let mut found_v4: Option<String> = None;
    let mut found_v6: Option<String> = None;
    let mut has_cgpaddr_line = false;

    for line in content.lines() {
        let line = line.trim();
        let Some(payload) = line.strip_prefix("+CGPADDR:") else {
            continue;
        };
        has_cgpaddr_line = true;

        let mut segments = payload.split(',').map(str::trim);
        let cid = segments
            .next()
            .map(|value| value.trim_matches('"'))
            .and_then(|value| value.parse::<u8>().ok());
        if cid != Some(target_cid) {
            continue;
        }

        for segment in segments {
            let clean_ip = segment.trim_matches(|c| c == '"' || c == ' ' || c == '\r' || c == '\n');

            if clean_ip.is_empty() || clean_ip == "0.0.0.0" || clean_ip == "::" {
                continue;
            }

            // MT5700M-CN 的 IPv6 地址以点分十进制格式返回（16个字节，共15个点）
            // 例如: "32.8.0.2.0.2.0.1.255.255.255.255.255.255.255.255"
            // 标准冒号格式: "2001:db8::1" 也兼容处理
            let dot_count = clean_ip.chars().filter(|&c| c == '.').count();
            let colon_count = clean_ip.chars().filter(|&c| c == ':').count();

            if colon_count >= 2 {
                // 标准 IPv6 冒号格式
                debug!("Detected IPv6 (colon fmt): {}", clean_ip);
                found_v6 = Some(clean_ip.to_string());
            } else if dot_count == 15 {
                // MT5700M-CN 点分十进制 IPv6 格式（16字节，15个点）
                // 验证所有段都是 0-255 的数字
                let all_valid = clean_ip.split('.').all(|s| s.parse::<u8>().is_ok());
                if all_valid {
                    debug!("Detected IPv6 (dotted-decimal fmt): {}", clean_ip);
                    found_v6 = Some(clean_ip.to_string());
                } else {
                    debug!("Detected IPv4: {}", clean_ip);
                    found_v4 = Some(clean_ip.to_string());
                }
            } else if clean_ip.contains('.') && dot_count == 3 {
                // 标准 IPv4 格式（x.x.x.x）
                debug!("Detected IPv4: {}", clean_ip);
                found_v4 = Some(clean_ip.to_string());
            }
        }
    }

    if !has_cgpaddr_line {
        return IpStatus::Unexpected;
    }

    match (found_v4, found_v6) {
        (Some(v4), Some(v6)) => IpStatus::DualStack(v4, v6),
        (Some(v4), None)     => IpStatus::Ipv4Only(v4),
        (None,     Some(v6)) => IpStatus::Ipv6Only(v6),
        (None,     None)     => IpStatus::NoIp,
    }
}

fn is_data_session_cid(cid: Option<u8>) -> bool {
    cid.map(|value| value == DATA_CID).unwrap_or(true)
}

/// 检查模组是否已被用户手动关闭自动拨号（AT^SETAUTODIAL=0）
/// 返回 true 表示已关闭，后端不应触发灾难恢复
async fn is_auto_dial_disabled(at_client: &ATClient) -> bool {
    let resp = at_client.send_command("AT^SETAUTODIAL?".to_string()).await;
    if let Ok(r) = resp {
        if let Some(data) = r.data {
            // Some firmware spells the prefix as SETAUTODAIL.
            return parse_auto_dial_enabled(&data)
                .map(|enabled| enabled == 0)
                .unwrap_or(false);
        }
    }
    false // 查询失败时保守处理，不阻止恢复
}

/// Recover only the modem-side data session. Router interface recovery is kept
/// separate so DHCP/default-route failures never cause an unnecessary redial.
async fn recover_modem_session(
    at_client: &ATClient,
    data_mode: DataMode,
    force_reset: bool,
) -> bool {
    warn!(
        "Recovering modem data session for {} mode (force_reset={}).",
        data_mode.label(),
        force_reset
    );

    if force_reset {
        let disconnect_command = format!("AT^NDISDUP={},0", DATA_CID);
        match at_client.send_command(disconnect_command).await {
            Ok(response) if response.success => sleep(Duration::from_secs(2)).await,
            Ok(response) => debug!(
                "Session disconnect command was not accepted: {}",
                response.error.unwrap_or_else(|| "unknown error".to_string())
            ),
            Err(e) => warn!("Failed to send session disconnect command: {}", e),
        }
    }

    let connect_command = format!("AT^NDISDUP={},1", DATA_CID);
    match at_client.send_command(connect_command).await {
        Ok(response) if response.success => {}
        Ok(response) => {
            warn!(
                "Session connect command failed: {}",
                response.error.unwrap_or_else(|| "unknown error".to_string())
            );
            return false;
        }
        Err(e) => {
            warn!("Failed to send session connect command: {}", e);
            return false;
        }
    }

    if wait_for_ip(at_client).await {
        info!("Modem data session recovered for {} mode.", data_mode.label());
        true
    } else {
        warn!("Timed out waiting for modem data-session address.");
        return false;
    }
}

fn drain_modem_session_events(
    receiver: &mut tokio::sync::broadcast::Receiver<crate::models::ModemSessionEvent>,
) {
    let mut drained = 0u32;
    loop {
        match receiver.try_recv() {
            Ok(_) => drained += 1,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => drained += n as u32,
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            | Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
        }
    }
    if drained > 0 {
        debug!("Drained {} session event(s) generated during recovery.", drained);
    }
}

/// 等待有效 IP，参考 QModem 增加 120 秒超时熔断
/// 返回 true 表示成功获取 IP，false 表示超时
async fn wait_for_ip(at_client: &ATClient) -> bool {
    debug!("Waiting for valid IP address (timeout: 120s)...");
    let max_retries = 60u32; // 60 * 2s = 120s
    let mut retries = 0u32;

    loop {
        if retries >= max_retries {
            error!("Timed out (120s) waiting for valid IP address.");
            return false;
        }
        match check_ip_status(at_client).await {
            Ok(status) if status.has_ip() => {
                debug!("IP successfully obtained.");
                return true;
            }
            Ok(IpStatus::Unexpected) => {
                warn!("Unexpected AT response while waiting for IP (retry {}/{}).", retries + 1, max_retries);
            }
            Ok(_) => {
                debug!("No IP yet, retrying ({}/{})...", retries + 1, max_retries);
            }
            Err(e) => {
                warn!("Error checking IP status: {}", e);
            }
        }
        retries += 1;
        sleep(Duration::from_secs(2)).await;
    }
}

async fn detect_data_interface(configured: &str, data_mode: DataMode) -> Result<DataInterface> {
    if !configured.is_empty() && configured != "auto" {
        if let Some(selection) = resolve_logical_interface(configured).await {
            return Ok(selection);
        }

        let path = format!("/sys/class/net/{}", configured);
        if fs::metadata(&path).await.is_err() {
            bail!("configured interface {} does not exist", configured);
        }
        return Ok(DataInterface::managed(configured.to_string()));
    }

    match data_mode {
        DataMode::Usb => detect_usb_modem_interface()
            .await
            .map(DataInterface::managed)
            .ok_or_else(|| {
                anyhow!("no USB virtual network interface with vendor ID 3466 was found")
            }),
        DataMode::Ethernet => {
            if let Some(selection) = resolve_logical_interface("wan").await {
                info!(
                    "Using native OpenWrt WAN interface {} (device {}).",
                    selection.logical_interface,
                    selection.device
                );
                return Ok(selection);
            }

            detect_native_gigabit_interface()
                .await
                .map(DataInterface::managed)
        }
    }
}

/// Resolve an existing OpenWrt logical interface to its kernel network device.
/// Reusing `wan` avoids starting a second DHCP client on the same physical port.
async fn resolve_logical_interface(interface: &str) -> Option<DataInterface> {
    let uci_key = format!("network.{}.device", interface);
    let configured_device = Command::new("uci")
        .args(["-q", "get", &uci_key])
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (!value.is_empty()).then_some(value)
        });

    if let Some(device) = configured_device {
        if fs::metadata(format!("/sys/class/net/{}", device)).await.is_ok() {
            return Some(DataInterface::existing(interface, device));
        }
    }

    let output = Command::new("ifstatus").arg(interface).output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let selection = interface_status_devices(&status).find_map(|device| {
        std::path::Path::new("/sys/class/net")
            .join(&device)
            .exists()
            .then(|| DataInterface::existing(interface, device))
    });
    selection
}

fn interface_status_devices(status: &serde_json::Value) -> impl Iterator<Item = String> + '_ {
    ["device", "l3_device"]
        .into_iter()
        .filter_map(|field| status.get(field)?.as_str().map(str::to_string))
}

/// USB data mode: find the virtual ECM/NCM interface below the modem USB device.
async fn detect_usb_modem_interface() -> Option<String> {
    let net_dir = "/sys/class/net";
    let Ok(mut entries) = fs::read_dir(net_dir).await else { return None; };

    // MT5700M-CN 专用：只匹配鼎桥 VID 3466
    let valid_vids = ["3466"];

    while let Ok(Some(entry)) = entries.next_entry().await {
        let iface = entry.file_name().into_string().unwrap_or_default();
        if iface == "lo" || iface.starts_with("br-") || iface.starts_with("wl") || iface.starts_with("ra") {
            continue;
        }

        let vendor_path_direct = format!("{}/{}/device/idVendor", net_dir, iface);
        let vendor_path_parent = format!("{}/{}/device/../idVendor", net_dir, iface);

        let mut vid = match fs::read_to_string(&vendor_path_direct).await {
            Ok(v) => v,
            Err(_) => String::new(),
        };
        if vid.trim().is_empty() {
            if let Ok(v) = fs::read_to_string(&vendor_path_parent).await {
                vid = v;
            }
        }

        let vid = vid.trim().to_lowercase();
        if !vid.is_empty() && valid_vids.contains(&vid.as_str()) {
            info!("Found USB modem data interface: {} (Vendor ID: {})", iface, vid);
            return Some(iface);
        }
    }

    warn!("No USB modem data interface found based on Vendor ID.");
    None
}

/// Fallback for systems without a resolvable `network.wan`: select a unique,
/// linked native gigabit port. Multi-WAN systems must configure the interface.
async fn detect_native_gigabit_interface() -> Result<String> {
    let net_dir = "/sys/class/net";
    let mut entries = fs::read_dir(net_dir).await?;
    let mut candidates = Vec::new();

    while let Some(entry) = entries.next_entry().await? {
        let iface = entry.file_name().into_string().unwrap_or_default();
        if should_skip_interface(&iface) {
            continue;
        }

        let base = format!("{}/{}", net_dir, iface);
        if fs::metadata(format!("{}/device", base)).await.is_err()
            || fs::metadata(format!("{}/master", base)).await.is_ok()
        {
            continue;
        }

        let device_path = match fs::canonicalize(format!("{}/device", base)).await {
            Ok(path) => path.to_string_lossy().to_lowercase(),
            Err(_) => continue,
        };
        if device_path.contains("/usb") {
            continue;
        }

        let carrier = fs::read_to_string(format!("{}/carrier", base))
            .await
            .unwrap_or_default();
        if carrier.trim() != "1" {
            continue;
        }

        let speed = fs::read_to_string(format!("{}/speed", base))
            .await
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok());
        if speed != Some(1000) {
            continue;
        }

        debug!(
            "Native gigabit Ethernet candidate: {} (speed=1000, path={})",
            iface,
            device_path
        );
        candidates.push(iface);
    }

    select_single_ethernet_candidate(candidates)
}

fn should_skip_interface(iface: &str) -> bool {
    iface == "lo"
        || iface.starts_with("br-")
        || iface.starts_with("wl")
        || iface.starts_with("ra")
        || iface.starts_with("usb")
        || iface.starts_with("wwan")
        || iface.starts_with("ppp")
        || iface.starts_with("tun")
        || iface.starts_with("tap")
        || iface.starts_with("veth")
        || iface.starts_with("docker")
}

fn select_single_ethernet_candidate(mut candidates: Vec<String>) -> Result<String> {
    candidates.sort();
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => bail!(
            "native OpenWrt interface wan could not be resolved and no active native gigabit port was found; configure option ifname explicitly"
        ),
        _ => bail!(
            "multiple active native gigabit Ethernet ports were found ({}); configure the modem WAN interface explicitly",
            candidates.join(", ")
        ),
    }
}

async fn detect_data_mode(at_client: &ATClient) -> Result<DataMode> {
    let response = at_client
        .send_command("AT^SETAUTODIAL?".to_string())
        .await?;
    if !response.success {
        bail!(
            "AT^SETAUTODIAL? failed: {}",
            response.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }

    let data = response
        .data
        .ok_or_else(|| anyhow!("AT^SETAUTODIAL? returned no data"))?;
    parse_data_mode(&data).ok_or_else(|| {
        anyhow!(
            "unsupported or malformed AT^SETAUTODIAL? response: {}",
            data.replace(['\r', '\n'], " ")
        )
    })
}

fn find_auto_dial_payload(data: &str) -> Option<&str> {
    data.lines().find_map(|line| {
        let line = line.trim();
        Some(line
            .strip_prefix("^SETAUTODIAL:")
            .or_else(|| line.strip_prefix("^SETAUTODAIL:"))?
            .trim())
    })
}

fn parse_auto_dial_enabled(data: &str) -> Option<u8> {
    find_auto_dial_payload(data)?
        .split(',')
        .next()?
        .trim()
        .parse::<u8>()
        .ok()
}

fn parse_auto_dial_fields(data: &str) -> Option<(u8, u8)> {
    let mut fields = find_auto_dial_payload(data)?.split(',').map(str::trim);
    let enabled = fields.next()?.parse::<u8>().ok()?;
    let mode = fields.next()?.parse::<u8>().ok()?;
    Some((enabled, mode))
}

fn parse_data_mode(data: &str) -> Option<DataMode> {
    match parse_auto_dial_fields(data)?.1 {
        1 => Some(DataMode::Usb),
        2 => Some(DataMode::Ethernet),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usb_data_mode() {
        assert_eq!(
            parse_data_mode("^SETAUTODIAL: 1,1,\"IPV4V6\"\r\nOK"),
            Some(DataMode::Usb)
        );
    }

    #[test]
    fn parses_ethernet_data_mode_and_firmware_typo() {
        assert_eq!(
            parse_data_mode("^SETAUTODAIL:0,2\r\nOK"),
            Some(DataMode::Ethernet)
        );
    }

    #[test]
    fn rejects_unknown_data_mode() {
        assert_eq!(parse_data_mode("^SETAUTODIAL: 1,9\r\nOK"), None);
    }

    #[test]
    fn parses_disabled_single_field_response() {
        assert_eq!(parse_auto_dial_enabled("^SETAUTODAIL: 0\r\nOK"), Some(0));
    }

    #[test]
    fn rejects_ambiguous_native_ethernet_candidates() {
        let result = select_single_ethernet_candidate(vec![
            "eth2".to_string(),
            "eth3".to_string(),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn prefers_physical_device_over_protocol_l3_device() {
        let status = serde_json::json!({
            "device": "eth1",
            "l3_device": "pppoe-wan"
        });
        assert_eq!(
            interface_status_devices(&status).collect::<Vec<_>>(),
            vec!["eth1".to_string(), "pppoe-wan".to_string()]
        );
    }

    #[test]
    fn parses_dual_stack_ndis_query_state() {
        assert_eq!(
            parse_ndis_query_state(
                "^NDISSTATQRY: 0,,,\"IPV4\",1,,,\"IPV6\"\r\nOK",
                DATA_CID,
            ),
            Some(ModemSessionState::Connected)
        );
        assert_eq!(
            parse_ndis_query_state(
                "^NDISSTATQRY: 0,,,\"IPV4\"\r\n^NDISSTATQRY: 2,,,\"IPV6\"\r\nOK",
                DATA_CID,
            ),
            Some(ModemSessionState::Connecting)
        );
    }

    #[test]
    fn parses_cid_prefixed_ndis_query_state() {
        assert_eq!(
            parse_ndis_query_state(
                "^NDISSTATQRY: 1,3,29,,\"IPV4V6\"\r\nOK",
                DATA_CID,
            ),
            Some(ModemSessionState::Disconnected)
        );
    }

    #[test]
    fn ndis_query_uses_only_the_data_cid() {
        assert_eq!(
            parse_ndis_query_state(
                "^NDISSTATQRY: 2,1,,,\"IPV4\"\r\n^NDISSTATQRY: 1,0,29,,\"IPV4V6\"\r\nOK",
                DATA_CID,
            ),
            Some(ModemSessionState::Disconnected)
        );
        assert_eq!(
            parse_ndis_query_state(
                "^NDISSTATQRY: 5,1,,,\"IPV4\"\r\nOK",
                DATA_CID,
            ),
            None
        );
    }

    #[test]
    fn cgpaddr_ignores_ims_and_other_cids() {
        let response = "+CGPADDR: 1\r\n+CGPADDR: 5,10.10.10.5\r\nOK";
        assert_eq!(parse_ip_status(response, DATA_CID), IpStatus::NoIp);
    }

    #[test]
    fn cgpaddr_parses_data_cid_dual_stack_addresses() {
        let response = concat!(
            "+CGPADDR: 5,10.10.10.5\r\n",
            "+CGPADDR: 1,100.64.1.2,",
            "32.8.0.2.0.2.0.1.255.255.255.255.255.255.255.255\r\n",
            "OK"
        );
        assert_eq!(
            parse_ip_status(response, DATA_CID),
            IpStatus::DualStack(
                "100.64.1.2".to_string(),
                "32.8.0.2.0.2.0.1.255.255.255.255.255.255.255.255".to_string(),
            )
        );
    }

    #[test]
    fn cgpaddr_without_result_lines_is_unexpected() {
        assert_eq!(parse_ip_status("OK", DATA_CID), IpStatus::Unexpected);
    }

    #[test]
    fn session_events_without_cid_target_the_primary_data_session() {
        assert!(is_data_session_cid(None));
        assert!(is_data_session_cid(Some(DATA_CID)));
        assert!(!is_data_session_cid(Some(5)));
    }

    #[test]
    fn usb_mode_treats_session_query_disconnect_as_authoritative() {
        assert_eq!(
            authoritative_session_status(
                DataMode::Usb,
                Some(ModemSessionState::Disconnected),
            ),
            Some(DataSessionStatus::Disconnected)
        );
        assert_eq!(
            authoritative_session_status(
                DataMode::Ethernet,
                Some(ModemSessionState::Disconnected),
            ),
            None
        );
    }

    #[test]
    fn ethernet_mode_accepts_data_cid_address_over_stale_query_state() {
        let ip_status = IpStatus::Ipv4Only("100.64.1.2".to_string());
        assert_eq!(
            resolve_ip_session_status(
                Some(ModemSessionState::Disconnected),
                ip_status.clone(),
            ),
            DataSessionStatus::Connected(ip_status)
        );
    }

    #[test]
    fn connected_session_without_data_cid_address_is_not_ready() {
        assert_eq!(
            resolve_ip_session_status(Some(ModemSessionState::Connected), IpStatus::NoIp),
            DataSessionStatus::NoAddress
        );
    }
}

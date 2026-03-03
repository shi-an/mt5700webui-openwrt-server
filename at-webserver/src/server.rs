use crate::client::ATClient;
use futures::{SinkExt, StreamExt};
use log::{error, info, debug, warn};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::Mutex;
use std::net::SocketAddr;
use tokio::sync::{oneshot, broadcast};
use tokio::time::{timeout, Duration};
use warp::Filter;
use std::sync::OnceLock;

pub static WS_BROADCASTER: OnceLock<broadcast::Sender<String>> = OnceLock::new();
pub static CLIENT_CONNECTIONS: OnceLock<Mutex<HashMap<SocketAddr, tokio::sync::mpsc::UnboundedSender<warp::ws::Message>>>> = OnceLock::new();

#[derive(Deserialize)]
struct WSCommand {
    command: String,
}

#[derive(Deserialize)]
struct AuthMessage {
    auth_key: String,
}

#[derive(Serialize)]
struct WSResponse {
    success: bool,
    data: Option<String>,
    error: Option<String>,
}

pub async fn start_server(
    _ipv4_port: u16,
    ipv6_port: u16,
    auth_key: Option<String>,
    at_client: ATClient,
    log_rx: broadcast::Receiver<String>,
    log_path: String,
) {
    let (ws_tx, _) = broadcast::channel(100);
    let _ = WS_BROADCASTER.set(ws_tx.clone());
    let _ = CLIENT_CONNECTIONS.set(Mutex::new(HashMap::new()));

    let at_client = Arc::new(at_client);
    let auth_key = Arc::new(auth_key);
    let log_rx = Arc::new(log_rx);
    let log_path = Arc::new(log_path);

    let at_client_filter = warp::any().map(move || at_client.clone());
    let auth_key_filter = warp::any().map(move || auth_key.clone());
    let log_rx_filter = warp::any().map(move || log_rx.clone());
    let log_path_filter = warp::any().map(move || log_path.clone());

    let routes = warp::path::end()
        .and(warp::ws())
        .and(warp::addr::remote())
        .and(at_client_filter)
        .and(auth_key_filter)
        .and(log_rx_filter)
        .and(log_path_filter)
        .map(|ws: warp::ws::Ws, addr: Option<SocketAddr>, client, key, rx, path| {
            ws.on_upgrade(move |socket| handle_client(socket, addr, client, key, rx, path))
        });

    info!("Starting WebSocket server on [::]:{} (Dual-stack IPv4 & IPv6)", ipv6_port);
    warp::serve(routes).run(([0, 0, 0, 0, 0, 0, 0, 0], ipv6_port)).await;
}

async fn handle_client(
    mut ws: warp::ws::WebSocket,
    addr: Option<SocketAddr>,
    at_client: Arc<ATClient>,
    auth_key: Arc<Option<String>>,
    log_rx: Arc<broadcast::Receiver<String>>,
    log_path: Arc<String>,
) {
    // Authentication
    if let Some(key) = auth_key.as_ref() {
        match timeout(Duration::from_secs(10), ws.next()).await {
            Ok(Some(Ok(msg))) => {
                if let Ok(text) = msg.to_str() {
                    let mut authenticated = false;
                    if let Ok(auth_data) = serde_json::from_str::<AuthMessage>(text) {
                        if &auth_data.auth_key == key {
                            authenticated = true;
                        }
                    }
                    
                    if authenticated {
                        let _ = ws.send(warp::ws::Message::text(json!({
                            "success": true,
                            "message": "认证成功"
                        }).to_string())).await;
                        debug!("WebSocket client authenticated");
                    } else {
                        let _ = ws.send(warp::ws::Message::text(json!({
                            "error": "Authentication failed",
                            "message": "密钥验证失败"
                        }).to_string())).await;
                        warn!("WebSocket authentication failed");
                        let _ = ws.close().await;
                        return;
                    }
                } else {
                    warn!("WebSocket received non-text auth message");
                    let _ = ws.close().await;
                    return;
                }
            }
            Ok(Some(Err(e))) => {
                error!("WebSocket auth error: {}", e);
                return;
            }
            Ok(None) => {
                warn!("WebSocket closed during auth");
                return;
            }
            Err(_) => {
                let _ = ws.send(warp::ws::Message::text(json!({
                    "error": "Authentication timeout",
                    "message": "认证超时"
                }).to_string())).await;
                warn!("WebSocket authentication timeout");
                let _ = ws.close().await;
                return;
            }
        }
    }

    let (mut tx, mut rx) = ws.split();
    let sender = at_client.get_sender();
    let mut log_rx = log_rx.resubscribe();
    let mut ws_raw_rx = WS_BROADCASTER.get().unwrap().subscribe();

    // 【步骤1】：新增一个专门用于异步接收后台 AT 指令结果的通道
    let (conn_tx, mut conn_rx) = tokio::sync::mpsc::channel::<String>(32);

    // Highlander Rule: Kick old connections from same IP
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<warp::ws::Message>();
    let cmd_tx_cleanup = cmd_tx.clone();
    if let Some(client_addr) = addr {
        if let Some(conns) = CLIENT_CONNECTIONS.get() {
            let mut conns = conns.lock().await;
            if let Some(old_tx) = conns.remove(&client_addr) {
                warn!("Detected new connection from {}, kicking old connection", client_addr);
                let _ = old_tx.send(warp::ws::Message::close());
            }
            conns.insert(client_addr, cmd_tx);
        }
    }

    // Start heartbeat loop (optional, but good for keepalive)
    // For warp/tungstenite, ping/pong is handled automatically at protocol level usually,
    // but python version sends 'ping' text. We can implement similar logic if needed.
    // Here we stick to standard WS protocol handling provided by library.

    loop {
        tokio::select! {
            // Handle global broadcast events (raw_data, new_sms, etc.)
            Ok(broadcast_msg) = ws_raw_rx.recv() => {
                 if let Err(e) = tx.send(warp::ws::Message::text(broadcast_msg)).await {
                     debug!("Failed to send broadcast to WS: {}", e);
                     break;
                 }
            }
            // 【步骤2】：监听后台发回的异步 AT 指令结果，并秒发给前端
            Some(resp_str) = conn_rx.recv() => {
                 if let Err(e) = tx.send(warp::ws::Message::text(resp_str)).await {
                     log::debug!("Failed to send async response to WS: {}", e);
                     break;
                 }
            }
            // Highlander Rule: Handle commands (like kick)
            Some(msg) = cmd_rx.recv() => {
                 let is_close = msg.is_close();
                 if let Err(e) = tx.send(msg).await {
                     log::debug!("Failed to send command to WS: {}", e);
                     break;
                 }
                 if is_close {
                     break;
                 }
            }
            // Handle incoming WS messages
            Some(result) = rx.next() => {
                match result {
                    Ok(msg) => {
                         let text = if msg.is_text() {
                             msg.to_str().unwrap_or("")
                         } else if msg.is_binary() {
                             std::str::from_utf8(msg.as_bytes()).unwrap_or("")
                         } else {
                             continue;
                         };

                         // 【复刻 Python】：直接回复纯文本 pong，且不被后续流程阻塞
                         if text.trim() == "ping" || text.is_empty() {
                             if let Err(e) = tx.send(warp::ws::Message::text("pong")).await {
                                 error!("Failed to send pong: {}", e);
                                 break;
                             }
                             continue;
                         }

                         // 3. 【终极容错解析】：智能判断并精准提取指令，绝不误伤指令自身的引号！
                         let mut cmd_str = String::new();
                         let text_trimmed = text.trim();
                         
                         // 尝试 1：当作完整的 JSON 对象解析 (比如 {"command": "AT+CFUN=0"})
                         if text_trimmed.starts_with('{') {
                             if let Ok(r) = serde_json::from_str::<WSCommand>(text_trimmed) {
                                 cmd_str = r.command;
                             }
                         }
                         
                         if cmd_str.is_empty() {
                             // 尝试 2：当作被 JSON.stringify 包装过的字符串解析 (完美处理转义符和外层引号)
                             if let Ok(s) = serde_json::from_str::<String>(text_trimmed) {
                                 cmd_str = s;
                             } else {
                                 // 尝试 3：纯得不能再纯的原始裸文本，直接用
                                 cmd_str = text_trimmed.to_string();
                             }
                         }

                         if cmd_str.is_empty() {
                             continue;
                         }

                         log::info!("WS Command: {}", cmd_str);

                         if cmd_str.trim() == "AT+CONNECT?" {
                             let resp = WSResponse { success: true, data: Some("+CONNECT: 0\r\nOK".to_string()), error: None };
                             let _ = tx.send(warp::ws::Message::text(serde_json::to_string(&resp).unwrap())).await;
                             continue;
                         }
                         
                         if cmd_str.trim() == "GET_SYS_LOGS" {
                             let content = match tokio::fs::read_to_string(log_path.as_str()).await {
                                 Ok(c) => c, Err(_) => "".to_string(),
                             };
                             let resp = WSResponse { success: true, data: Some(content), error: None };
                             let _ = tx.send(warp::ws::Message::text(serde_json::to_string(&resp).unwrap())).await;
                             continue;
                         }

                         if cmd_str.trim() == "CLEAR_SYS_LOGS" {
                             let success = tokio::fs::write(log_path.as_str(), "").await.is_ok();
                             let resp = WSResponse {
                                 success, data: if success { Some("Logs cleared".to_string()) } else { None },
                                 error: if success { None } else { Some("Failed to clear logs".to_string()) },
                             };
                             let _ = tx.send(warp::ws::Message::text(serde_json::to_string(&resp).unwrap())).await;
                             continue;
                         }

                         if cmd_str.starts_with("AT^SYSCFGEX") {
                             cmd_str = cmd_str.replace('\n', "").replace('\r', "").replace("OK", "");
                             if cmd_str.contains(",\"\",\"\"") {
                                 let parts: Vec<&str> = cmd_str.split(',').collect();
                                 if parts.len() >= 5 {
                                     let bands = parts[4].trim_matches('"');
                                     cmd_str = format!("{},{},{},{},\"{}\",\"\",\"\"", parts[0], parts[1], parts[2], parts[3], bands);
                                 }
                             }
                             cmd_str.push('\r');
                         }
                         
                         // 【异步并发】：将指令发给后端执行，主循环立刻回头去接客，绝不卡死 WebSocket！
                         let sender_clone = sender.clone();
                         // WebSocket 发送端 (tx) 通常不能直接克隆 (SplitSink 没有 Clone)。
                         // 我们这里使用之前创建的 conn_tx 通道将结果发回主循环，由主循环统一发送给 WebSocket。
                         let conn_tx_clone = conn_tx.clone();
                         let cmd_for_task = cmd_str.clone();
                         
                         tokio::spawn(async move {
                             let (resp_tx, resp_rx) = oneshot::channel();
                             if let Err(e) = sender_clone.send((cmd_for_task.clone(), resp_tx)).await {
                                 error!("Failed to send command to actor: {}", e);
                                 return;
                             }

                             match resp_rx.await {
                                 Ok(response) => {
                                     let mut filtered_data = response.data.clone();
                                     if let Some(data) = &filtered_data {
                                         let clean_cmd = cmd_for_task.trim();
                                         let lines: Vec<&str> = data.lines()
                                             .filter(|line| !line.trim().is_empty() && line.trim() != clean_cmd)
                                             .collect();
                                         filtered_data = Some(lines.join("\r\n"));
                                     }
                                     let ws_resp = WSResponse {
                                         success: response.success,
                                         data: filtered_data,
                                         error: response.error,
                                     };
                                     if let Ok(json_resp) = serde_json::to_string(&ws_resp) {
                                         let _ = conn_tx_clone.send(json_resp).await;
                                     }
                                 }
                                 Err(e) => {
                                     error!("Failed to receive response from actor: {}", e);
                                     let err_resp = json!({ "success": false, "error": "Internal Error" });
                                     let _ = conn_tx_clone.send(err_resp.to_string()).await;
                                 }
                             }
                         });
                    }
                    Err(e) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                }
            }
            else => break,
        }
    }
    info!("WebSocket client disconnected");
    if let Some(client_addr) = addr {
        if let Some(conns) = CLIENT_CONNECTIONS.get() {
            let mut conns = conns.lock().await;
            if let Some(sender) = conns.get(&client_addr) {
                if sender.same_channel(&cmd_tx_cleanup) {
                    conns.remove(&client_addr);
                }
            }
        }
    }
}

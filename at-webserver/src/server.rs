use crate::client::ATClient;
use futures::{SinkExt, StreamExt};
use log::{error, info, debug, warn};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::{oneshot, broadcast};
use tokio::time::{timeout, Duration};
use warp::Filter;

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
    ipv4_port: u16,
    ipv6_port: u16,
    auth_key: Option<String>,
    at_client: ATClient,
    log_rx: broadcast::Receiver<String>,
    log_path: String,
) {
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
        .and(at_client_filter)
        .and(auth_key_filter)
        .and(log_rx_filter)
        .and(log_path_filter)
        .map(|ws: warp::ws::Ws, client, key, rx, path| {
            ws.on_upgrade(move |socket| handle_client(socket, client, key, rx, path))
        });

    info!("Starting WebSocket server on [::]:{} (Dual-stack IPv4 & IPv6)", ipv6_port);
    warp::serve(routes).run(([0, 0, 0, 0, 0, 0, 0, 0], ipv6_port)).await;
}

async fn handle_client(
    mut ws: warp::ws::WebSocket,
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

    // Start heartbeat loop (optional, but good for keepalive)
    // For warp/tungstenite, ping/pong is handled automatically at protocol level usually,
    // but python version sends 'ping' text. We can implement similar logic if needed.
    // Here we stick to standard WS protocol handling provided by library.

    loop {
        tokio::select! {
            // Handle log messages
            Ok(log_msg) = log_rx.recv() => {
                 let msg = json!({
                     "type": "system_log",
                     "data": log_msg
                 }).to_string();
                 if let Err(e) = tx.send(warp::ws::Message::text(msg)).await {
                     // If client disconnected, we might get error here
                     debug!("Failed to send log to WS: {}", e);
                     break;
                 }
            }
            // Handle incoming WS messages
            Some(result) = rx.next() => {
                match result {
                    Ok(msg) => {
                        if msg.is_text() {
                            if let Ok(text) = msg.to_str() {
                                // Handle manual ping
                                if text == "ping" {
                                    if let Err(e) = tx.send(warp::ws::Message::text("pong")).await {
                                        error!("Failed to send pong: {}", e);
                                        break;
                                    }
                                    continue;
                                }

                                let mut cmd_str = if let Ok(json_cmd) = serde_json::from_str::<WSCommand>(text) {
                                    json_cmd.command
                                } else {
                                    text.to_string() // Assume raw string
                                };

                                info!("WS Command: {}", cmd_str);

                                // Special handling for AT+CONNECT?
                                if cmd_str.trim() == "AT+CONNECT?" {
                                    // Assume connected for now or implement check
                                    // In Python: returns +CONNECT: 0 (Network) or 1 (Serial)
                                    // We can hardcode 0 for now as most use network
                                    let resp = WSResponse {
                                        success: true,
                                        data: Some("+CONNECT: 0\r\nOK".to_string()),
                                        error: None,
                                    };
                                    let _ = tx.send(warp::ws::Message::text(serde_json::to_string(&resp).unwrap())).await;
                                    continue;
                                }
                                
                                // Handle GET_SYS_LOGS
                                if cmd_str.trim() == "GET_SYS_LOGS" {
                                    let content = match tokio::fs::read_to_string(log_path.as_str()).await {
                                        Ok(c) => c,
                                        Err(_) => "".to_string(),
                                    };
                                    // Wrap in standard AT response format or just raw? 
                                    // Python version returned raw content usually, but here we use WSResponse wrapper
                                    let resp = WSResponse {
                                        success: true,
                                        data: Some(content),
                                        error: None,
                                    };
                                    let _ = tx.send(warp::ws::Message::text(serde_json::to_string(&resp).unwrap())).await;
                                    continue;
                                }

                                // Handle CLEAR_SYS_LOGS
                                if cmd_str.trim() == "CLEAR_SYS_LOGS" {
                                    let success = tokio::fs::write(log_path.as_str(), "").await.is_ok();
                                    let resp = WSResponse {
                                        success,
                                        data: if success { Some("Logs cleared".to_string()) } else { None },
                                        error: if success { None } else { Some("Failed to clear logs".to_string()) },
                                    };
                                    let _ = tx.send(warp::ws::Message::text(serde_json::to_string(&resp).unwrap())).await;
                                    continue;
                                }

                                // Filter/Sanitize AT^SYSCFGEX
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
                                
                                let (resp_tx, resp_rx) = oneshot::channel();
                                if let Err(e) = sender.send((cmd_str.clone(), resp_tx)).await {
                                    error!("Failed to send command to actor: {}", e);
                                    break;
                                }

                                match resp_rx.await {
                                    Ok(response) => {
                                        // Filter out echoed command from response if present
                                        let mut filtered_data = response.data.clone();
                                        if let Some(data) = &filtered_data {
                                            let clean_cmd = cmd_str.trim();
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

                                        let json_resp = serde_json::to_string(&ws_resp).unwrap();
                                        if let Err(e) = tx.send(warp::ws::Message::text(json_resp)).await {
                                            error!("Failed to send response to WS: {}", e);
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        error!("Failed to receive response from actor: {}", e);
                                        let err_resp = json!({ "success": false, "error": "Internal Error" });
                                        let _ = tx.send(warp::ws::Message::text(err_resp.to_string())).await;
                                    }
                                }
                            }
                        }
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
}

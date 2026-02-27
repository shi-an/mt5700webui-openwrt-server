use crate::client::ATClient;
use crate::models::ATResponse;
use futures::{SinkExt, StreamExt};
use log::{error, info, debug, warn};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::oneshot;
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
) {
    let at_client = Arc::new(at_client);
    let auth_key = Arc::new(auth_key);

    let at_client_filter = warp::any().map(move || at_client.clone());
    let auth_key_filter = warp::any().map(move || auth_key.clone());

    let routes = warp::path::end()
        .and(warp::ws())
        .and(at_client_filter)
        .and(auth_key_filter)
        .map(|ws: warp::ws::Ws, client, key| {
            ws.on_upgrade(move |socket| handle_client(socket, client, key))
        });

    // Start IPv4 server
    let routes_v4 = routes.clone();
    let server_v4 = warp::serve(routes_v4).run(([0, 0, 0, 0], ipv4_port));

    // Start IPv6 server
    let server_v6 = warp::serve(routes).run(([0, 0, 0, 0, 0, 0, 0, 0], ipv6_port));

    info!("Starting WebSocket server on IPv4 0.0.0.0:{} and IPv6 [::]:{}", ipv4_port, ipv6_port);
    
    // Run both servers concurrently
    tokio::join!(server_v4, server_v6);
}

async fn handle_client(
    mut ws: warp::ws::WebSocket,
    at_client: Arc<ATClient>,
    auth_key: Arc<Option<String>>,
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

    // Start heartbeat loop (optional, but good for keepalive)
    // For warp/tungstenite, ping/pong is handled automatically at protocol level usually,
    // but python version sends 'ping' text. We can implement similar logic if needed.
    // Here we stick to standard WS protocol handling provided by library.

    while let Some(result) = rx.next().await {
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
    info!("WebSocket client disconnected");
}

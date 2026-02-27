use crate::client::ATClient;
use futures::{SinkExt, StreamExt};
use log::{error, info};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::oneshot;
use warp::Filter;

#[derive(Deserialize)]
struct WSCommand {
    command: String,
}

#[derive(Serialize)]
struct WSResponse {
    success: bool,
    data: Option<String>,
    error: Option<String>,
}

pub async fn start_server(port: u16, at_client: ATClient) {
    let at_client = warp::any().map(move || at_client.clone());

    // Path /ws or root? Python serves at /? No, API doc says `GET /cgi-bin/at-ws-info` returns ws url.
    // The WS URL is likely just `ws://host:port/`.
    // We will bind to root.

    let routes = warp::path::end()
        .and(warp::ws())
        .and(at_client)
        .map(|ws: warp::ws::Ws, client| {
            ws.on_upgrade(move |socket| handle_client(socket, client))
        });

    info!("Starting WebSocket server on 0.0.0.0:{}", port);
    warp::serve(routes).run(([0, 0, 0, 0], port)).await;
}

async fn handle_client(ws: warp::ws::WebSocket, at_client: ATClient) {
    let (mut tx, mut rx) = ws.split();
    let sender = at_client.get_sender();

    while let Some(result) = rx.next().await {
        match result {
            Ok(msg) => {
                if msg.is_text() {
                    if let Ok(text) = msg.to_str() {
                        let cmd_str = if let Ok(json_cmd) = serde_json::from_str::<WSCommand>(text) {
                            json_cmd.command
                        } else {
                            text.to_string() // Assume raw string
                        };

                        info!("WS Command: {}", cmd_str);
                        
                        let (resp_tx, resp_rx) = oneshot::channel();
                        if let Err(e) = sender.send((cmd_str, resp_tx)).await {
                            error!("Failed to send command to actor: {}", e);
                            break;
                        }

                        match resp_rx.await {
                            Ok(response) => {
                                let json_resp = serde_json::to_string(&response).unwrap();
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

use crate::config::Config;
use crate::connection::{ATConnection, NetworkATConnection, SerialATConnection};
use crate::handlers::{CallHandler, MemoryFullHandler, MessageHandler, NetworkSignalHandler, NewSMSHandler, PDCPDataHandler};
use crate::models::{ATResponse, CommandSender, ConnectionType};
use crate::notifications::NotificationManager;
use log::{error, info, warn, debug};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, timeout};

#[derive(Clone)]
pub struct ATClient {
    tx: CommandSender,
}

impl ATClient {
    pub fn new(config: Config, notifications: NotificationManager) -> Self {
        let (tx, rx) = mpsc::channel(32);
        let tx_clone = tx.clone();
        
        tokio::spawn(async move {
            let mut actor = ATClientActor::new(config, notifications, rx, tx_clone);
            actor.run().await;
        });

        Self { tx }
    }

    pub fn get_sender(&self) -> CommandSender {
        self.tx.clone()
    }

    pub async fn send_command(&self, cmd: String) -> anyhow::Result<ATResponse> {
        let (tx, rx) = oneshot::channel();
        self.tx.send((cmd, tx)).await.map_err(|_| anyhow::anyhow!("Failed to send command"))?;
        match rx.await {
            Ok(resp) => Ok(resp),
            Err(_) => Err(anyhow::anyhow!("Failed to receive response")),
        }
    }
}

struct ATClientActor {
    config: Config,
    notifications: NotificationManager,
    rx: mpsc::Receiver<(String, oneshot::Sender<ATResponse>)>,
    connection: Option<Box<dyn ATConnection>>,
    handlers: Vec<Box<dyn MessageHandler>>,
    cmd_tx: CommandSender,
    buffer: Vec<u8>,
    urc_tx: mpsc::Sender<String>, // 新增专门用于分发 URC 的通道
}

impl ATClientActor {
    fn new(
        config: Config, 
        notifications: NotificationManager, 
        rx: mpsc::Receiver<(String, oneshot::Sender<ATResponse>)>,
        cmd_tx: CommandSender,
    ) -> Self {
        // 建立一个解耦的 URC 分发通道
        let (urc_tx, mut urc_rx) = mpsc::channel::<String>(100);
        let notifs = notifications.clone();
        let cmd_tx_clone = cmd_tx.clone();
        
        // 【解除死锁的核心】：在独立的后台协程中处理 URC，防止 Handler 再次发送 AT 指令时阻塞主 Actor
        tokio::spawn(async move {
            let mut async_handlers: Vec<Box<dyn MessageHandler>> = vec![
                Box::new(CallHandler),
                Box::new(MemoryFullHandler),
                Box::new(NewSMSHandler),
                Box::new(PDCPDataHandler),
                Box::new(NetworkSignalHandler),
            ];
            while let Some(line) = urc_rx.recv().await {
                for handler in &mut async_handlers {
                    if handler.can_handle(&line) {
                        let _ = handler.handle(&line, &notifs, &cmd_tx_clone).await;
                    }
                }
            }
        });

        let handlers: Vec<Box<dyn MessageHandler>> = vec![
            Box::new(CallHandler),
            Box::new(MemoryFullHandler),
            Box::new(NewSMSHandler),
            Box::new(PDCPDataHandler),
            Box::new(NetworkSignalHandler),
        ];

        Self {
            config,
            notifications,
            rx,
            connection: None,
            handlers,
            cmd_tx,
            buffer: Vec::new(),
            urc_tx,
        }
    }

    async fn run(&mut self) {
        loop {
            if self.connection.is_none() || !self.connection.as_ref().unwrap().is_connected() {
                if !self.connect().await {
                    sleep(Duration::from_secs(5)).await;
                    continue;
                }
            }
            
            self.process_loop().await;
            sleep(Duration::from_secs(1)).await;
        }
    }

    async fn connect(&mut self) -> bool {
        let mut connection: Box<dyn ATConnection> = match self.config.at_config.connection_type {
            ConnectionType::Network => {
                Box::new(NetworkATConnection::new(
                    self.config.at_config.network.host.clone(),
                    self.config.at_config.network.port,
                    self.config.at_config.network.timeout,
                ))
            },
            ConnectionType::Serial => {
                Box::new(SerialATConnection::new(
                    self.config.at_config.serial.port.clone(),
                    self.config.at_config.serial.baudrate,
                ))
            }
        };

        match connection.connect().await {
            Ok(_) => {
                self.connection = Some(connection);
                true
            }
            Err(e) => {
                error!("Failed to connect: {}", e);
                false
            }
        }
    }

    async fn process_loop(&mut self) {
        let mut buf = [0u8; 1024];

        loop {
            if let Some(conn) = &self.connection {
                if !conn.is_connected() { break; }
            } else { break; }

            // Using select with inline access to avoid multiple mutable borrows of self
            let rx = &mut self.rx;
            let conn = self.connection.as_mut().unwrap();

            tokio::select! {
                Some((cmd, reply_tx)) = rx.recv() => {
                    // Check if connected
                    // We need to release `conn` before calling helper that might use other fields?
                    // Actually, we should just handle sending here or pass `conn` to helper.
                    // To satisfy borrow checker, we pass `conn` and `&mut self.buffer` etc. separately.
                    
                    if let Err(e) = Self::send_command_and_wait(
                        conn, 
                        &mut self.buffer, 
                        &self.handlers, 
                        &self.notifications, 
                        &self.cmd_tx, 
                        &self.urc_tx,
                        cmd, 
                        reply_tx
                    ).await {
                         error!("Error processing command: {}", e);
                         if e.to_string().contains("Closed") || e.to_string().contains("Not connected") {
                            self.connection = None;
                            break; 
                         }
                    }
                }
                res = conn.receive(&mut buf) => {
                    match res {
                        Ok(n) if n > 0 => {
                            self.buffer.extend_from_slice(&buf[..n]);
                            Self::process_buffer_lines(
                                &mut self.buffer, 
                                &self.handlers, 
                                &self.notifications, 
                                &self.cmd_tx,
                                &self.urc_tx
                            ).await;
                        }
                        Ok(_) => {
                            warn!("Connection closed (EOF)");
                            self.connection = None;
                            break;
                        }
                        Err(e) => {
                            error!("Read error: {}", e);
                            self.connection = None;
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn send_command_and_wait(
        conn: &mut Box<dyn ATConnection>,
        buffer: &mut Vec<u8>,
        handlers: &[Box<dyn MessageHandler>],
        _notifications: &NotificationManager,
        _cmd_tx: &CommandSender,
        urc_tx: &mpsc::Sender<String>,
        cmd: String,
        reply_tx: oneshot::Sender<ATResponse>
    ) -> anyhow::Result<()> {
        
        // 1. 发射前清膛：抽干底层 OS 缓存，防止上一条高频指令遗留的 OK 破坏本次解析
        let mut buf = [0u8; 1024];
        while let Ok(Ok(n)) = timeout(Duration::from_millis(5), conn.receive(&mut buf)).await {
            if n == 0 { break; }
            buffer.extend_from_slice(&buf[..n]);
        }
        
        // 把抽出来的残余数据当作空闲状态处理掉
        while let Some(line) = extract_next_line(buffer) {
            let _ = urc_tx.send(line.clone()).await;
            if let Some(tx) = crate::server::WS_BROADCASTER.get() {
                let _ = tx.send(serde_json::json!({"type": "raw_data", "data": line}).to_string());
            }
        }

        // 2. 硬件保护：强制限制 AT 指令并发速率（模块串口处理需要时间）
        tokio::time::sleep(Duration::from_millis(100)).await;
        let clean_cmd = cmd.trim();
        info!("Sending Command: {}", clean_cmd);
        conn.send(clean_cmd.as_bytes()).await?;
        conn.send(b"\r\n").await?;

        let start = std::time::Instant::now();
        let timeout_dur = Duration::from_secs(10);
        let mut response_data = String::new();
        
        loop {
            if start.elapsed() > timeout_dur {
                let _ = reply_tx.send(ATResponse::error("Timeout".to_string()));
                return Ok(());
            }

            match timeout(Duration::from_secs(1), conn.receive(&mut buf)).await {
                Ok(Ok(n)) => {
                    if n == 0 { 
                         let _ = reply_tx.send(ATResponse::error("Connection closed".to_string()));
                         anyhow::bail!("Closed");
                    }
                    buffer.extend_from_slice(&buf[..n]);
                    
                    while let Some(line) = extract_next_line(buffer) {
                        debug!("RCV: {}", line);
                        
                        // 3. 【切断死循环核心】：如果是 URC（主动通知），才广播 raw_data
                        if Self::is_urc(handlers, &line) {
                            let _ = urc_tx.send(line.clone()).await;
                            if let Some(tx) = crate::server::WS_BROADCASTER.get() {
                                let ws_msg = serde_json::json!({
                                    "type": "raw_data",
                                    "data": line
                                }).to_string();
                                let _ = tx.send(ws_msg);
                            }
                            continue;
                        }
                        // 绝不能在这里盲目广播 raw_data，正常的响应只存入 response_data
                        if line == "OK" {
                             response_data.push_str("OK");
                             let _ = reply_tx.send(ATResponse::ok(Some(response_data)));
                             return Ok(());
                        } else if line.contains("ERROR") {
                             response_data.push_str(&line);
                             let _ = reply_tx.send(ATResponse::error(response_data));
                             return Ok(());
                        } else if line.starts_with(">") {
                             response_data.push_str(&line);
                             let _ = reply_tx.send(ATResponse::ok(Some(response_data))); 
                             return Ok(());
                        } else {
                             response_data.push_str(&line);
                             response_data.push_str("\r\n");
                        }
                    }
                },
                Ok(Err(e)) => {
                     let _ = reply_tx.send(ATResponse::error(e.to_string()));
                     return Err(e);
                },
                Err(_) => {}
            }
        }
    }

    async fn process_buffer_lines(
        buffer: &mut Vec<u8>,
        handlers: &[Box<dyn MessageHandler>],
        _notifications: &NotificationManager,
        _cmd_tx: &CommandSender,
        urc_tx: &mpsc::Sender<String>
    ) {
         while let Some(line) = extract_next_line(buffer) {
             debug!("URC/Idle: {}", line);
             
             // 广播所有原始数据给 Vue 前端，用于驱动信号图表和 AT 终端
             if let Some(tx) = crate::server::WS_BROADCASTER.get() {
                 let ws_msg = serde_json::json!({
                     "type": "raw_data",
                     "data": line.clone()
                 }).to_string();
                 let _ = tx.send(ws_msg);
             }

             if Self::is_urc(handlers, &line) {
                 let _ = urc_tx.send(line).await;
             }
         }
    }

    fn is_urc(handlers: &[Box<dyn MessageHandler>], line: &str) -> bool {
        for handler in handlers {
            if handler.can_handle(line) { return true; }
        }
        false
    }
}

fn extract_next_line(buffer: &mut Vec<u8>) -> Option<String> {
    if let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
        // 直接使用切片读取，避免 collect 产生额外的 Vec<u8> 内存分配
        let line = String::from_utf8_lossy(&buffer[..pos]).trim().to_string();
        // 直接丢弃已读字节
        buffer.drain(..=pos);
        
        if line.is_empty() {
            return extract_next_line(buffer);
        }
        return Some(line);
    }
    None
}

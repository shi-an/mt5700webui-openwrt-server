use crate::config::Config;
use crate::connection::{ATConnection, NetworkATConnection, SerialATConnection};
use crate::handlers::{CallHandler, MemoryFullHandler, MessageHandler, NewSMSHandler};
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
}

struct ATClientActor {
    config: Config,
    notifications: NotificationManager,
    rx: mpsc::Receiver<(String, oneshot::Sender<ATResponse>)>,
    connection: Option<Box<dyn ATConnection>>,
    handlers: Vec<Box<dyn MessageHandler>>,
    cmd_tx: CommandSender,
    buffer: Vec<u8>,
}

impl ATClientActor {
    fn new(
        config: Config, 
        notifications: NotificationManager, 
        rx: mpsc::Receiver<(String, oneshot::Sender<ATResponse>)>,
        cmd_tx: CommandSender,
    ) -> Self {
        let handlers: Vec<Box<dyn MessageHandler>> = vec![
            Box::new(CallHandler),
            Box::new(MemoryFullHandler),
            Box::new(NewSMSHandler),
        ];

        Self {
            config,
            notifications,
            rx,
            connection: None,
            handlers,
            cmd_tx,
            buffer: Vec::new(),
        }
    }

    async fn connect(&mut self) -> bool {
        let mut conn: Box<dyn ATConnection> = match self.config.at_config.connection_type {
            ConnectionType::Network => {
                Box::new(NetworkATConnection::new(
                    self.config.at_config.network.host.clone(),
                    self.config.at_config.network.port,
                    self.config.at_config.network.timeout,
                ))
            }
            ConnectionType::Serial => {
                Box::new(SerialATConnection::new(
                    self.config.at_config.serial.port.clone(),
                    self.config.at_config.serial.baudrate,
                ))
            }
        };

        match conn.connect().await {
            Ok(_) => {
                self.connection = Some(conn);
                info!("AT Client connected");
                true
            }
            Err(e) => {
                error!("Connection failed: {}", e);
                false
            }
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
                                &self.cmd_tx
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
        notifications: &NotificationManager,
        cmd_tx: &CommandSender,
        cmd: String,
        reply_tx: oneshot::Sender<ATResponse>
    ) -> anyhow::Result<()> {
        
        info!("Sending Command: {}", cmd);
        conn.send(cmd.as_bytes()).await?;
        conn.send(b"\r\n").await?;

        let start = std::time::Instant::now();
        let timeout_dur = Duration::from_secs(10);
        let mut response_data = String::new();
        let mut buf = [0u8; 1024];

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
                        if line == "OK" {
                             let _ = reply_tx.send(ATResponse::ok(Some(response_data.trim().to_string())));
                             return Ok(());
                        } else if line.contains("ERROR") {
                             let _ = reply_tx.send(ATResponse::error(format!("AT Error: {}", line)));
                             return Ok(());
                        } else if line.starts_with(">") {
                             let _ = reply_tx.send(ATResponse::ok(Some(response_data.trim().to_string()))); 
                             return Ok(());
                        } else if Self::is_urc(handlers, &line) {
                            Self::handle_urc(handlers, &line, notifications, cmd_tx).await;
                        } else {
                            response_data.push_str(&line);
                            response_data.push('\n');
                        }
                    }
                },
                Ok(Err(e)) => {
                     let _ = reply_tx.send(ATResponse::error(e.to_string()));
                     return Err(e);
                },
                Err(_) => {
                    // Timeout per read
                }
            }
        }
    }

    async fn process_buffer_lines(
        buffer: &mut Vec<u8>,
        handlers: &[Box<dyn MessageHandler>],
        notifications: &NotificationManager,
        cmd_tx: &CommandSender
    ) {
         while let Some(line) = extract_next_line(buffer) {
             debug!("URC/Idle: {}", line);
             Self::handle_urc(handlers, &line, notifications, cmd_tx).await;
         }
    }

    fn is_urc(handlers: &[Box<dyn MessageHandler>], line: &str) -> bool {
        for handler in handlers {
            if handler.can_handle(line) { return true; }
        }
        false
    }
    
    async fn handle_urc(
        handlers: &[Box<dyn MessageHandler>], 
        line: &str, 
        notifications: &NotificationManager, 
        cmd_tx: &CommandSender
    ) {
        for handler in handlers {
            if handler.can_handle(line) {
                if let Err(e) = handler.handle(line, notifications, cmd_tx).await {
                    error!("Handler error: {}", e);
                }
            }
        }
    }
}

fn extract_next_line(buffer: &mut Vec<u8>) -> Option<String> {
    if let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
         let remaining = buffer.split_off(pos + 1);
         let mut line_bytes = buffer.clone();
         *buffer = remaining; // update buffer to remaining
         
         // Remove \n and optional \r
         if let Some(&last) = line_bytes.last() {
             if last == b'\n' { line_bytes.pop(); }
         }
         if let Some(&last) = line_bytes.last() {
             if last == b'\r' { line_bytes.pop(); }
         }

         let line = String::from_utf8_lossy(&line_bytes).trim().to_string();
         if line.is_empty() { return extract_next_line(buffer); }
         return Some(line);
    }
    None
}

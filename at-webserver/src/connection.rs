use async_trait::async_trait;
use anyhow::{Result, Context};
use log::info;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tokio_serial::SerialPortBuilderExt;

#[async_trait]
pub trait ATConnection: Send {
    async fn connect(&mut self) -> Result<()>;
    async fn close(&mut self) -> Result<()>;
    async fn send(&mut self, data: &[u8]) -> Result<()>;
    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize>;
    fn is_connected(&self) -> bool;
}

pub struct NetworkATConnection {
    host: String,
    port: u16,
    timeout_secs: u64,
    stream: Option<TcpStream>,
}

impl NetworkATConnection {
    pub fn new(host: String, port: u16, timeout_secs: u64) -> Self {
        Self {
            host,
            port,
            timeout_secs,
            stream: None,
        }
    }
}

#[async_trait]
impl ATConnection for NetworkATConnection {
    async fn connect(&mut self) -> Result<()> {
        let addr = format!("{}:{}", self.host, self.port);
        info!("Connecting to network AT server at {}", addr);
        match timeout(Duration::from_secs(self.timeout_secs), TcpStream::connect(&addr)).await {
            Ok(result) => {
                self.stream = Some(result.context("Failed to connect to network AT server")?);
                info!("Connected to network AT server");
                Ok(())
            }
            Err(_) => {
                anyhow::bail!("Connection timed out");
            }
        }
    }

    async fn close(&mut self) -> Result<()> {
        if let Some(mut stream) = self.stream.take() {
            let _ = stream.shutdown().await;
        }
        Ok(())
    }

    async fn send(&mut self, data: &[u8]) -> Result<()> {
        if let Some(stream) = &mut self.stream {
            stream.write_all(data).await.context("Failed to write to stream")?;
            stream.flush().await.context("Failed to flush stream")?;
            Ok(())
        } else {
            anyhow::bail!("Not connected");
        }
    }

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if let Some(stream) = &mut self.stream {
            // We just await data. Cancellation via timeout is handled by caller (client.rs: select!)
            stream.read(buffer).await.context("Failed to read from stream")
        } else {
            anyhow::bail!("Not connected");
        }
    }

    fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
}

pub struct SerialATConnection {
    port: String,
    baudrate: u32,
    stream: Option<tokio_serial::SerialStream>,
}

impl SerialATConnection {
    pub fn new(port: String, baudrate: u32) -> Self {
        Self {
            port,
            baudrate,
            stream: None,
        }
    }
}

#[async_trait]
impl ATConnection for SerialATConnection {
    async fn connect(&mut self) -> Result<()> {
        info!("Opening serial port {} at {}", self.port, self.baudrate);
        let port = tokio_serial::new(&self.port, self.baudrate)
            .open_native_async()
            .context("Failed to open serial port")?;
        self.stream = Some(port);
        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        self.stream = None;
        Ok(())
    }

    async fn send(&mut self, data: &[u8]) -> Result<()> {
        if let Some(stream) = &mut self.stream {
            stream.write_all(data).await.context("Failed to write to serial")?;
            stream.flush().await.context("Failed to flush serial")?;
            Ok(())
        } else {
            anyhow::bail!("Not connected");
        }
    }

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if let Some(stream) = &mut self.stream {
             // Serial reading doesn't inherently timeout in the same way, but we can wrap it.
             // Usually we just read.
             stream.read(buffer).await.context("Failed to read from serial")
        } else {
            anyhow::bail!("Not connected");
        }
    }

     fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
}

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::{oneshot, Mutex};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt};
use futures::StreamExt;

/// Sidechannel registry key type
type RegistryKey = String;

/// Gitaly uses yamux to multiplex gRPC and sidechannel streams over a single TCP/unix connection.
/// This struct manages the yamux connection and routes incoming sidechannel streams.
pub struct GitalyConnection {
    control: Arc<Mutex<yamux::Control>>,
    registry: Arc<Mutex<HashMap<RegistryKey, oneshot::Sender<Sidechannel>>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl GitalyConnection {
    pub async fn connect_tcp(addr: &str) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Self::from_stream(stream).await
    }

    pub async fn connect_unix(path: &str) -> io::Result<Self> {
        let stream = UnixStream::connect(path).await?;
        Self::from_stream(stream).await
    }

    async fn from_stream(stream: impl AsyncRead + AsyncWrite + Send + Unpin + 'static) -> io::Result<Self> {
        let cfg = yamux::Config::default();
        let compat = stream.compat();
        let connection = yamux::Connection::new(compat, cfg, yamux::Mode::Client);
        let control = connection.control();
        let registry: Arc<Mutex<HashMap<RegistryKey, oneshot::Sender<Sidechannel>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let reg = registry.clone();
        let task = tokio::spawn(async move {
            let mut conn = connection;
            while let Some(result) = conn.next().await {
                match result {
                    Ok(raw_stream) => {
                        let compat_stream = raw_stream.compat();
                        let mut sidechannel = Sidechannel { stream: compat_stream };
                        // Read registry key (first 32-byte UUID hex string)
                        let mut key_buf = [0u8; 32];
                        if sidechannel.read_exact(&mut key_buf).await.is_ok() {
                            let key = String::from_utf8_lossy(&key_buf).to_string();
                            let mut reg_map = reg.lock().await;
                            if let Some(tx) = reg_map.remove(&key) {
                                let _ = tx.send(sidechannel);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!("yamux connection error: {}", e);
                        break;
                    }
                }
            }
        });

        Ok(Self {
            control: Arc::new(Mutex::new(control)),
            registry,
            _task: task,
        })
    }

    /// Open a new yamux stream for tonic's gRPC transport (client-initiated)
    pub async fn open_compat_stream(&self) -> io::Result<Compat<yamux::Stream>> {
        let mut ctrl = self.control.lock().await;
        let stream = ctrl
            .open_stream()
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        Ok(stream.compat())
    }

    /// Register a sidechannel waiter and return the registry key.
    /// The key must be sent to Gitaly via gRPC metadata so Gitaly can route
    /// the sidechannel stream back to us.
    pub async fn register_sidechannel_waiter(&self) -> (RegistryKey, oneshot::Receiver<Sidechannel>) {
        let key = uuid::Uuid::new_v4().to_string().replace('-', "");
        let (tx, rx) = oneshot::channel();
        self.registry.lock().await.insert(key.clone(), tx);
        (key, rx)
    }

    pub fn control(&self) -> Arc<Mutex<yamux::Control>> {
        self.control.clone()
    }
}

/// A sidechannel stream for bidirectional git protocol data exchange.
/// Received from Gitaly after PostUploadPackWithSidechannel.
pub struct Sidechannel {
    stream: Compat<yamux::Stream>,
}

impl Sidechannel {
    pub async fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.stream.write_all(data).await
    }

    pub async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        self.stream.read_to_end(buf).await
    }

    pub async fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.stream.read_exact(buf).await
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        self.stream.shutdown().await
    }
}

impl AsyncRead for Sidechannel {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for Sidechannel {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

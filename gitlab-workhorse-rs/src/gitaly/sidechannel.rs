use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::{oneshot, Mutex as TokioMutex};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt};

type RegistryKey = String;

pub struct GitalyConnection {
    inner: Arc<std::sync::Mutex<yamux::Connection<Compat<CompatStream>>>>,
    registry: Arc<TokioMutex<HashMap<RegistryKey, oneshot::Sender<Sidechannel>>>>,
    _task: tokio::task::JoinHandle<()>,
}

enum CompatStream {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl tokio::io::AsyncRead for CompatStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            CompatStream::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            CompatStream::Unix(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for CompatStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            CompatStream::Tcp(s) => Pin::new(s).poll_write(cx, buf),
            CompatStream::Unix(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            CompatStream::Tcp(s) => Pin::new(s).poll_flush(cx),
            CompatStream::Unix(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            CompatStream::Tcp(s) => Pin::new(s).poll_shutdown(cx),
            CompatStream::Unix(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

impl GitalyConnection {
    pub async fn connect_tcp(addr: &str) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Self::from_stream(CompatStream::Tcp(stream)).await
    }

    pub async fn connect_unix(path: &str) -> io::Result<Self> {
        let stream = UnixStream::connect(path).await?;
        Self::from_stream(CompatStream::Unix(stream)).await
    }

    async fn from_stream(stream: CompatStream) -> io::Result<Self> {
        let compat: Compat<CompatStream> = Compat::new(stream);
        let cfg = yamux::Config::default();
        let connection = yamux::Connection::new(compat, cfg, yamux::Mode::Client);
        let conn = Arc::new(std::sync::Mutex::new(connection));
        let registry: Arc<TokioMutex<HashMap<RegistryKey, oneshot::Sender<Sidechannel>>>> =
            Arc::new(TokioMutex::new(HashMap::new()));

        let reg = registry.clone();
        let c = conn.clone();
        let task = tokio::spawn(async move {
            loop {
                let maybe_stream = {
                    let mut guard = c.lock().unwrap();
                    let waker = futures::task::noop_waker_ref();
                    let mut cx = Context::from_waker(waker);
                    match guard.poll_next_inbound(&mut cx) {
                        Poll::Ready(Some(Ok(stream))) => Some(stream),
                        Poll::Ready(Some(Err(e))) => {
                            tracing::debug!("yamux inbound error: {}", e);
                            None
                        }
                        Poll::Ready(None) => {
                            tracing::debug!("yamux connection closed");
                            return;
                        }
                        Poll::Pending => None,
                    }
                };

                if let Some(mut raw_stream) = maybe_stream {
                    let mut key_buf = [0u8; 32];
                    let mut read = 0usize;
                    loop {
                        let waker = futures::task::noop_waker_ref();
                        let mut cx = Context::from_waker(waker);
                        let buf_slice = &mut key_buf[read..];
                        match futures::AsyncRead::poll_read(
                            Pin::new(&mut raw_stream),
                            &mut cx,
                            buf_slice,
                        ) {
                            Poll::Ready(Ok(0)) => break,
                            Poll::Ready(Ok(n)) => {
                                read += n;
                                if read >= 32 {
                                    break;
                                }
                            }
                            Poll::Ready(Err(_)) => break,
                            Poll::Pending => {
                                tokio::task::yield_now().await;
                            }
                        }
                    }

                    if read == 32 {
                        let key = String::from_utf8_lossy(&key_buf).to_string();
                        let mut reg_map = reg.lock().await;
                        if let Some(tx) = reg_map.remove(&key) {
                            let _ = tx.send(Sidechannel { stream: raw_stream });
                        }
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            }
        });

        Ok(Self {
            inner: conn,
            registry,
            _task: task,
        })
    }

    /// Open a new yamux stream. Returns a futures::io compatible stream.
    /// This usually succeeds immediately (no I/O needed for allocating a new stream ID).
    pub async fn open_yamux_stream(&self) -> io::Result<yamux::Stream> {
        let c = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            loop {
                let mut guard = c.lock().unwrap();
                let waker = futures::task::noop_waker_ref();
                let mut cx = Context::from_waker(waker);
                match guard.poll_new_outbound(&mut cx) {
                    Poll::Ready(Ok(stream)) => return Ok(stream),
                    Poll::Ready(Err(e)) => return Err(io::Error::new(io::ErrorKind::Other, format!("{}", e))),
                    Poll::Pending => {
                        drop(guard);
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
            }
        })
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{}", e)))?
    }

    /// Open a new yamux stream wrapped in tokio-compatible Compat adapter.
    pub async fn open_compat_stream(&self) -> io::Result<Compat<yamux::Stream>> {
        let stream = self.open_yamux_stream().await?;
        Ok(stream.compat())
    }

    pub async fn register_sidechannel_waiter(&self) -> (RegistryKey, oneshot::Receiver<Sidechannel>) {
        let key = uuid::Uuid::new_v4().to_string().replace('-', "");
        let (tx, rx) = oneshot::channel();
        self.registry.lock().await.insert(key.clone(), tx);
        (key, rx)
    }
}

pub struct Sidechannel {
    stream: yamux::Stream,
}

impl Sidechannel {
    pub async fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        futures::AsyncWriteExt::write_all(Pin::new(&mut self.stream), data).await
    }

    pub async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let before = buf.len();
        futures::AsyncReadExt::read_to_end(Pin::new(&mut self.stream), buf).await
    }

    pub async fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        futures::AsyncReadExt::read_exact(Pin::new(&mut self.stream), buf).await
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        futures::AsyncWriteExt::close(Pin::new(&mut self.stream)).await
    }
}

impl tokio::io::AsyncRead for Sidechannel {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let inner_buf = buf.initialize_unfilled();
        match futures::AsyncRead::poll_read(
            Pin::new(&mut self.stream),
            cx,
            inner_buf,
        ) {
            Poll::Ready(Ok(n)) => {
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl tokio::io::AsyncWrite for Sidechannel {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        futures::AsyncWrite::poll_write(Pin::new(&mut self.stream), cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        futures::AsyncWrite::poll_flush(Pin::new(&mut self.stream), cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        futures::AsyncWrite::poll_close(Pin::new(&mut self.stream), cx)
    }
}

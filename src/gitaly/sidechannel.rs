
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};
use futures::prelude::*;
use futures::FutureExt;

const BACKCHANNEL_MAGIC: &[u8; 11] = b"backchannel";
const SIDECHANNEL_MAGIC: &[u8; 11] = b"sidechannel";

// Wrapper to implement futures AsyncRead/AsyncWrite for tokio streams
struct TokioCompat<T>(T);

impl<T: tokio::io::AsyncRead + Unpin> futures::io::AsyncRead for TokioCompat<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let mut read_buf = tokio::io::ReadBuf::new(buf);
        match Pin::new(&mut self.0).poll_read(cx, &mut read_buf) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(read_buf.filled().len())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T: tokio::io::AsyncWrite + Unpin> futures::io::AsyncWrite for TokioCompat<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

// Wrapper to implement tokio AsyncRead/AsyncWrite for futures streams
pub struct FuturesCompat<T>(T);

impl<T: futures::io::AsyncRead + Unpin> tokio::io::AsyncRead for FuturesCompat<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let unfilled = buf.initialize_unfilled();
        match Pin::new(&mut self.0).poll_read(cx, unfilled) {
            Poll::Ready(Ok(n)) => {
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T: futures::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for FuturesCompat<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_close(cx)
    }
}

impl<T: futures::io::AsyncRead + Unpin> hyper::rt::Read for FuturesCompat<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        // Create a temporary buffer to read into
        let mut temp_buf = vec![0u8; buf.remaining()];
        
        match Pin::new(&mut self.0).poll_read(cx, &mut temp_buf) {
            Poll::Ready(Ok(n)) => {
                if n > 0 {
                    buf.put_slice(&temp_buf[..n]);
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T: futures::io::AsyncWrite + Unpin> hyper::rt::Write for FuturesCompat<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_close(cx)
    }
}

enum CompatStream {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl tokio::io::AsyncRead for CompatStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            CompatStream::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            CompatStream::Unix(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for CompatStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
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

pub struct YamuxSession {
    stream_req_tx: mpsc::UnboundedSender<oneshot::Sender<yamux::Stream>>,
    sidechannel_waiter: Arc<TokioMutex<Option<oneshot::Sender<SidechannelStream>>>>,
}

impl YamuxSession {
    pub async fn connect(addr: &str) -> io::Result<Self> {
        tracing::info!("yamux connect: connecting to {}", addr);
        let raw = connect_raw(addr).await?;
        tracing::info!("yamux connect: raw connection established");

        // Write backchannel magic bytes
        let mut raw = raw;
        tokio::io::AsyncWriteExt::write_all(&mut raw, BACKCHANNEL_MAGIC).await?;
        tracing::info!("yamux connect: backchannel magic sent");

        // Create yamux client session with Gitaly-compatible configuration
        let mut config = yamux::Config::default();
        config.set_max_num_streams(256);
        config.set_split_send_size(16 * 1024); // 16KB
        
        tracing::info!("yamux config: max_streams={}, split_size={}", 
            256,
            16 * 1024
        );
        
        let wrapped = TokioCompat(raw);
        let mut connection = yamux::Connection::new(wrapped, config, yamux::Mode::Client);
        tracing::info!("yamux connect: yamux client session created");

        // Use a channel to communicate between the main task and the driver
        let (stream_req_tx, mut stream_req_rx) = mpsc::unbounded_channel::<oneshot::Sender<yamux::Stream>>();
        
        // Single sidechannel waiter per session (since Gitaly allocates its own ID)
        let sidechannel_waiter: Arc<TokioMutex<Option<oneshot::Sender<SidechannelStream>>>> =
            Arc::new(TokioMutex::new(None));
        let sidechannel_waiter_clone = sidechannel_waiter.clone();
        
        // Spawn the connection driver
        // The driver uses now_or_never() for non-blocking polling and spawns separate tasks
        // for sidechannel stream handling to avoid blocking the connection driver loop
        tokio::spawn(async move {
            let mut pending_opener: Option<oneshot::Sender<yamux::Stream>> = None;
            
            loop {
                // First, try to open a stream if we have a pending request
                if let Some(ref _opener) = pending_opener {
                    let can_open = futures::future::poll_fn(|cx| {
                        Pin::new(&mut connection).poll_new_outbound(cx)
                    }).now_or_never();
                    
                    if let Some(result) = can_open {
                        match result {
                            Ok(stream) => {
                                tracing::info!("yamux driver: outbound stream opened");
                                let opener = pending_opener.take().unwrap();
                                let _ = opener.send(stream);
                            }
                            Err(e) => {
                                tracing::error!("yamux driver: failed to open stream: {}", e);
                                break;
                            }
                        }
                    }
                }
                
                // Check for stream open requests
                match stream_req_rx.try_recv() {
                    Ok(opener) => {
                        tracing::debug!("yamux driver: received open request");
                        pending_opener = Some(opener);
                        continue;
                    }
                    Err(mpsc::error::TryRecvError::Empty) => {}
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        tracing::info!("yamux driver: stream request channel closed");
                        break;
                    }
                }
                
                // Try to accept inbound streams (non-blocking check)
                let inbound = futures::future::poll_fn(|cx| {
                    Pin::new(&mut connection).poll_next_inbound(cx)
                }).now_or_never();
                
                match inbound {
                    Some(Some(Ok(mut stream))) => {
                        tracing::info!("yamux driver: accepted incoming stream");
                        
                        // Spawn a separate task to handle the sidechannel protocol
                        let sc_waiter = sidechannel_waiter_clone.clone();
                        tokio::spawn(async move {
                            // Read sidechannel magic bytes
                            let mut magic_buf = [0u8; 11];
                            if let Err(e) = futures::io::AsyncReadExt::read_exact(&mut stream, &mut magic_buf).await {
                                tracing::error!("sidechannel handler: failed to read magic: {}", e);
                                return;
                            }
                            tracing::info!("sidechannel handler: read magic: {:?}", magic_buf);
                            
                            if &magic_buf != SIDECHANNEL_MAGIC {
                                tracing::error!("sidechannel handler: invalid magic: {:?}", magic_buf);
                                return;
                            }

                            // Read sidechannel ID (we read it but don't use it for matching)
                            let mut sid_bytes = [0u8; 8];
                            if let Err(e) = futures::io::AsyncReadExt::read_exact(&mut stream, &mut sid_bytes).await {
                                tracing::error!("sidechannel handler: failed to read sid: {}", e);
                                return;
                            }
                            let sid = u64::from_be_bytes(sid_bytes);
                            tracing::info!("sidechannel handler: sid={}", sid);

                            // Send "ok" confirmation to Gitaly
                            if let Err(e) = futures::io::AsyncWriteExt::write_all(&mut stream, b"ok").await {
                                tracing::error!("sidechannel handler: failed to send ok: {}", e);
                                return;
                            }
                            tracing::info!("sidechannel handler: ok sent for sid={}", sid);

                            // Flush to ensure "ok" is sent
                            if let Err(e) = futures::io::AsyncWriteExt::flush(&mut stream).await {
                                tracing::error!("sidechannel handler: failed to flush: {}", e);
                                return;
                            }

                            // Create sidechannel stream and notify the single waiter
                            let sidechannel = SidechannelStream::new(stream);
                            if let Some(waiter_tx) = sc_waiter.lock().await.take() {
                                tracing::info!("sidechannel handler: notifying waiter for sid={}", sid);
                                let _ = waiter_tx.send(sidechannel);
                            } else {
                                tracing::warn!("sidechannel handler: no waiter registered");
                            }
                        });
                    }
                    Some(Some(Err(e))) => {
                        tracing::error!("yamux driver: accept stream error: {}", e);
                        break;
                    }
                    Some(None) => {
                        tracing::info!("yamux driver: connection closed");
                        break;
                    }
                    None => {
                        // No inbound streams available, sleep briefly to avoid busy loop
                        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    }
                }
            }
            
            tracing::info!("yamux driver: task finished");
        });

        Ok(Self {
            stream_req_tx,
            sidechannel_waiter,
        })
    }

    pub async fn register_sidechannel(&self) -> io::Result<(String, oneshot::Receiver<SidechannelStream>)> {
        let mut key_bytes = [0u8; 32];
        let u1 = uuid::Uuid::new_v4();
        let u2 = uuid::Uuid::new_v4();
        key_bytes[..16].copy_from_slice(u1.as_bytes());
        key_bytes[16..].copy_from_slice(u2.as_bytes());
        let key_hex = hex::encode(&key_bytes);

        let (tx, rx) = oneshot::channel();
        *self.sidechannel_waiter.lock().await = Some(tx);

        Ok((key_hex, rx))
    }

    pub async fn create_grpc_channel(&self) -> io::Result<tonic::transport::Channel> {
        // Create a connector that opens new streams from the yamux session
        let stream_req_tx = self.stream_req_tx.clone();
        
        // Create a custom connector that returns yamux streams
        let connector = tower::service_fn(move |_uri: tonic::transport::Uri| {
            let stream_req_tx = stream_req_tx.clone();
            async move {
                // Request a new stream from the yamux session
                let (stream_tx, stream_rx) = oneshot::channel();
                stream_req_tx.send(stream_tx)
                    .map_err(|_| io::Error::new(io::ErrorKind::Other, "failed to request stream"))?;
                
                let stream = stream_rx.await
                    .map_err(|_| io::Error::new(io::ErrorKind::Other, "failed to get stream"))?;
                
                // Wrap the yamux stream in FuturesCompat to make it compatible with tokio
                let compat_stream = FuturesCompat(stream);
                
                Ok::<_, io::Error>(compat_stream)
            }
        });
        
        // Create a tonic channel using the custom connector
        // Use a dummy URI since we're using a custom connector
        let endpoint = tonic::transport::Endpoint::from_static("http://localhost");
        let channel = endpoint.connect_with_connector(connector).await
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("connect: {}", e)))?;
        
        Ok(channel)
    }
}

pub struct SidechannelStream {
    stream: yamux::Stream,
    read_buf: Vec<u8>,
    read_pos: usize,
}

impl SidechannelStream {
    pub fn new(stream: yamux::Stream) -> Self {
        Self {
            stream,
            read_buf: Vec::new(),
            read_pos: 0,
        }
    }

    pub async fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        futures::io::AsyncWriteExt::write_all(&mut self.stream, data).await
    }

    /// Write data wrapped in a pktline frame: [4-byte hex length][data]
    /// This matches Gitaly's ClientConn.Write() framing protocol.
    /// The length is a 4-byte hex string representing the total length including the 4-byte prefix.
    pub async fn write_pktline_framed(&mut self, data: &[u8]) -> io::Result<()> {
        let total_len = data.len() + 4; // include the 4-byte length prefix itself
        let header = format!("{:04x}", total_len);
        futures::io::AsyncWriteExt::write_all(&mut self.stream, header.as_bytes()).await?;
        futures::io::AsyncWriteExt::write_all(&mut self.stream, data).await?;
        Ok(())
    }

    /// Close the write side of the connection (equivalent to ClientConn.CloseWrite()).
    /// Sends a pktline flush packet "0000" to signal half-close.
    pub async fn close_write(&mut self) -> io::Result<()> {
        // Send flush packet "0000" as per pktline protocol
        futures::io::AsyncWriteExt::write_all(&mut self.stream, b"0000").await?;
        futures::io::AsyncWriteExt::flush(&mut self.stream).await?;
        Ok(())
    }

    pub async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        self.read_to_end_with_limit(buf, 100 * 1024 * 1024).await
    }

    pub async fn read_to_end_with_limit(&mut self, buf: &mut Vec<u8>, max_bytes: usize) -> io::Result<usize> {
        let before = buf.len();
        let mut temp_buf = vec![0u8; 8192];
        loop {
            match futures::io::AsyncReadExt::read(&mut self.stream, &mut temp_buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() - before + n > max_bytes {
                        return Err(io::Error::new(io::ErrorKind::Other, "sidechannel read exceeded size limit"));
                    }
                    buf.extend_from_slice(&temp_buf[..n]);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(buf.len() - before)
    }

    pub async fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        futures::io::AsyncReadExt::read_exact(&mut self.stream, buf).await
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        // Flush before closing to ensure all data is sent
        futures::io::AsyncWriteExt::flush(&mut self.stream).await?;
        futures::io::AsyncWriteExt::close(&mut self.stream).await
    }

    pub async fn flush(&mut self) -> io::Result<()> {
        futures::io::AsyncWriteExt::flush(&mut self.stream).await
    }
}

impl tokio::io::AsyncRead for SidechannelStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let inner = buf.initialize_unfilled();
        if inner.is_empty() {
            return Poll::Ready(Ok(()));
        }
        
        // Use the read buffer to handle partial reads
        while self.read_pos >= self.read_buf.len() {
            self.read_buf.clear();
            self.read_pos = 0;
            
            // Try to read from the stream
            let mut temp_buf = vec![0u8; 8192];
            match Pin::new(&mut self.stream).poll_read(cx, &mut temp_buf) {
                Poll::Ready(Ok(0)) => return Poll::Ready(Ok(())),
                Poll::Ready(Ok(n)) => {
                    self.read_buf = temp_buf[..n].to_vec();
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        
        let available = &self.read_buf[self.read_pos..];
        let n = available.len().min(inner.len());
        inner[..n].copy_from_slice(&available[..n]);
        self.read_pos += n;
        buf.advance(n);
        Poll::Ready(Ok(()))
    }
}

impl tokio::io::AsyncWrite for SidechannelStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_close(cx)
    }
}

async fn connect_raw(addr: &str) -> io::Result<CompatStream> {
    if addr.starts_with("unix:") {
        let path = addr.trim_start_matches("unix:");
        Ok(CompatStream::Unix(UnixStream::connect(path).await?))
    } else {
        Ok(CompatStream::Tcp(TcpStream::connect(addr).await?))
    }
}

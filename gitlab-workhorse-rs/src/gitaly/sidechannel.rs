use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};

const YAMUX_TYPE_DATA: u8 = 0;
const YAMUX_TYPE_WINDOW_UPDATE: u8 = 1;
const YAMUX_FLAG_SYN: u16 = 0x1;
const YAMUX_FLAG_ACK: u16 = 0x2;
const YAMUX_FLAG_FIN: u16 = 0x4;
const YAMUX_FLAG_RST: u16 = 0x8;

const BACKCHANNEL_MAGIC: &[u8; 11] = b"backchannel";
const SIDECHANNEL_MAGIC: &[u8; 11] = b"sidechannel";

struct YamuxFrame {
    stream_id: u32,
    frame_type: u8,
    flags: u16,
    data: Vec<u8>,
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

type RawConn = Arc<TokioMutex<CompatStream>>;

pub struct YamuxSession {
    raw: RawConn,
    write_tx: mpsc::UnboundedSender<(u32, Vec<u8>)>,
    stream_rx_map: Arc<TokioMutex<HashMap<u32, mpsc::UnboundedSender<Vec<u8>>>>>,
    sidechannel_waiters: Arc<TokioMutex<HashMap<u64, oneshot::Sender<SidechannelStream>>>>,
    grpc_rx: TokioMutex<Option<mpsc::UnboundedReceiver<Vec<u8>>>>,
}

impl YamuxSession {
    pub async fn connect(addr: &str) -> io::Result<Self> {
        let mut raw = connect_raw(addr).await?;

        raw.write_all(BACKCHANNEL_MAGIC).await?;

        send_raw_frame(&mut raw, YAMUX_TYPE_DATA, YAMUX_FLAG_SYN, 0, &[]).await?;
        let frame = read_raw_frame(&mut raw).await?;
        if frame.stream_id != 0 || frame.flags != (YAMUX_FLAG_SYN | YAMUX_FLAG_ACK) {
            return Err(io::Error::new(io::ErrorKind::Other, "yamux handshake failed"));
        }

        let raw: RawConn = Arc::new(TokioMutex::new(raw));
        let (write_tx, write_rx) = mpsc::unbounded_channel::<(u32, Vec<u8>)>();
        let stream_rx_map = Arc::new(TokioMutex::new(HashMap::new()));
        let sidechannel_waiters: Arc<TokioMutex<HashMap<u64, oneshot::Sender<SidechannelStream>>>> =
            Arc::new(TokioMutex::new(HashMap::new()));

        let (s1_tx, s1_rx) = mpsc::unbounded_channel();
        stream_rx_map.lock().await.insert(1, s1_tx);

        let grpc_rx = TokioMutex::new(Some(s1_rx));

        {
            let mut guard = raw.lock().await;
            send_raw_frame(&mut *guard, YAMUX_TYPE_DATA, YAMUX_FLAG_SYN, 1, &[]).await?;
            let win: u32 = 0x4000_0000;
            send_raw_frame(&mut *guard, YAMUX_TYPE_WINDOW_UPDATE, 0, 1, &win.to_be_bytes()).await?;
        }

        spawn_write_task(raw.clone(), write_rx);
        spawn_read_task(
            raw.clone(),
            stream_rx_map.clone(),
            sidechannel_waiters.clone(),
            write_tx.clone(),
        );

        Ok(Self {
            raw,
            write_tx,
            stream_rx_map,
            sidechannel_waiters,
            grpc_rx,
        })
    }

    pub async fn take_grpc_stream(&self) -> io::Result<YamuxStream> {
        let rx = self.grpc_rx.lock().await.take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "grpc stream already taken"))?;
        Ok(YamuxStream {
            write_tx: self.write_tx.clone(),
            rx,
            stream_id: 1,
            read_buf: Vec::new(),
            read_pos: 0,
        })
    }

    pub async fn register_sidechannel(&self) -> io::Result<(String, oneshot::Receiver<SidechannelStream>)> {
        let mut key_bytes = [0u8; 32];
        let u1 = uuid::Uuid::new_v4();
        let u2 = uuid::Uuid::new_v4();
        key_bytes[..16].copy_from_slice(u1.as_bytes());
        key_bytes[16..].copy_from_slice(u2.as_bytes());
        let key_hex = hex::encode(&key_bytes);

        let sid = u64::from_be_bytes(key_bytes[..8].try_into().unwrap());
        let (tx, rx) = oneshot::channel();
        self.sidechannel_waiters.lock().await.insert(sid, tx);

        Ok((key_hex, rx))
    }

    pub fn write_tx(&self) -> mpsc::UnboundedSender<(u32, Vec<u8>)> {
        self.write_tx.clone()
    }
}

fn spawn_write_task(raw: RawConn, mut rx: mpsc::UnboundedReceiver<(u32, Vec<u8>)>) {
    tokio::spawn(async move {
        while let Some((stream_id, data)) = rx.recv().await {
            let mut guard = raw.lock().await;
            if let Err(e) = send_raw_frame(&mut *guard, YAMUX_TYPE_DATA, 0, stream_id, &data).await {
                tracing::debug!("yamux write error on stream {}: {}", stream_id, e);
                break;
            }
        }
    });
}

fn spawn_read_task(
    raw: RawConn,
    stream_rx_map: Arc<TokioMutex<HashMap<u32, mpsc::UnboundedSender<Vec<u8>>>>>,
    sidechannel_waiters: Arc<TokioMutex<HashMap<u64, oneshot::Sender<SidechannelStream>>>>,
    write_tx: mpsc::UnboundedSender<(u32, Vec<u8>)>,
) {
    tokio::spawn(async move {
        loop {
            let frame = {
                let mut guard = raw.lock().await;
                match read_raw_frame(&mut *guard).await {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::debug!("yamux read error: {}", e);
                        break;
                    }
                }
            };

            if frame.frame_type == YAMUX_TYPE_WINDOW_UPDATE {
                continue;
            }

            if (frame.flags & YAMUX_FLAG_SYN) != 0
                && frame.stream_id > 0
                && frame.stream_id % 2 == 0
            {
                let stream_id = frame.stream_id;

                {
                    let mut guard = raw.lock().await;
                    let _ = send_raw_frame(&mut *guard, YAMUX_TYPE_DATA, YAMUX_FLAG_SYN | YAMUX_FLAG_ACK, stream_id, &[]).await;
                    let win: u32 = 0x4000_0000;
                    let _ = send_raw_frame(&mut *guard, YAMUX_TYPE_WINDOW_UPDATE, 0, stream_id, &win.to_be_bytes()).await;
                }

                let initial_data = {
                    let mut guard = raw.lock().await;
                    match read_raw_frame(&mut *guard).await {
                        Ok(f) if f.stream_id == stream_id => f.data,
                        _ => continue,
                    }
                };

                if initial_data.len() >= 11 && &initial_data[..11] == SIDECHANNEL_MAGIC {
                    let mut sid_bytes = [0u8; 8];
                    if initial_data.len() >= 19 {
                        sid_bytes.copy_from_slice(&initial_data[11..19]);
                    }
                    let sid = u64::from_be_bytes(sid_bytes);

                    {
                        let mut guard = raw.lock().await;
                        let _ = send_raw_frame(&mut *guard, YAMUX_TYPE_DATA, 0, stream_id, b"ok").await;
                    }

                    let (sc_tx, sc_rx) = mpsc::unbounded_channel();
                    {
                        stream_rx_map.lock().await.insert(stream_id, sc_tx);
                    }

                    let sidechannel = SidechannelStream {
                        rx: sc_rx,
                        write_tx: write_tx.clone(),
                        stream_id,
                        eof: false,
                    };

                    if let Some(waiter_tx) = sidechannel_waiters.lock().await.remove(&sid) {
                        let _ = waiter_tx.send(sidechannel);
                    }
                } else {
                    let (tx, _rx) = mpsc::unbounded_channel();
                    stream_rx_map.lock().await.insert(stream_id, tx);
                }
                continue;
            }

            if (frame.flags & YAMUX_FLAG_RST) != 0 {
                stream_rx_map.lock().await.remove(&frame.stream_id);
                continue;
            }

            if !frame.data.is_empty() || (frame.flags & YAMUX_FLAG_FIN) != 0 {
                let map = stream_rx_map.lock().await;
                if let Some(tx) = map.get(&frame.stream_id) {
                    if !frame.data.is_empty() {
                        let _ = tx.send(frame.data);
                    }
                }
                if (frame.flags & YAMUX_FLAG_FIN) != 0 {
                    drop(map);
                    stream_rx_map.lock().await.remove(&frame.stream_id);
                }
            }
        }
    });
}

pub struct YamuxStream {
    write_tx: mpsc::UnboundedSender<(u32, Vec<u8>)>,
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    stream_id: u32,
    read_buf: Vec<u8>,
    read_pos: usize,
}

impl tokio::io::AsyncRead for YamuxStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let inner = buf.initialize_unfilled();
        if inner.is_empty() {
            return Poll::Ready(Ok(()));
        }

        while self.read_pos >= self.read_buf.len() {
            self.read_buf.clear();
            self.read_pos = 0;
            match self.rx.poll_recv(cx) {
                Poll::Ready(Some(data)) => {
                    self.read_buf = data;
                }
                Poll::Ready(None) => {
                    return Poll::Ready(Ok(()));
                }
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

impl tokio::io::AsyncWrite for YamuxStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let _ = self.write_tx.send((self.stream_id, buf.to_vec()));
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub struct SidechannelStream {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    write_tx: mpsc::UnboundedSender<(u32, Vec<u8>)>,
    stream_id: u32,
    eof: bool,
}

impl SidechannelStream {
    pub async fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        let _ = self.write_tx.send((self.stream_id, data.to_vec()));
        Ok(())
    }

    pub async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let before = buf.len();
        while let Some(data) = self.rx.recv().await {
            buf.extend_from_slice(&data);
        }
        Ok(buf.len() - before)
    }

    pub async fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let mut offset = 0;
        while offset < buf.len() {
            match self.rx.recv().await {
                Some(data) => {
                    let n = data.len().min(buf.len() - offset);
                    buf[offset..offset + n].copy_from_slice(&data[..n]);
                    offset += n;
                }
                None => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "sidechannel closed")),
            }
        }
        Ok(())
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        self.eof = true;
        Ok(())
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
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(data)) => {
                let n = data.len().min(inner.len());
                inner[..n].copy_from_slice(&data[..n]);
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl tokio::io::AsyncWrite for SidechannelStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let _ = self.write_tx.send((self.stream_id, buf.to_vec()));
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
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

async fn send_raw_frame(
    stream: &mut CompatStream,
    type_: u8,
    flags: u16,
    stream_id: u32,
    data: &[u8],
) -> io::Result<()> {
    let mut header = [0u8; 12];
    header[1] = type_;
    header[2..4].copy_from_slice(&flags.to_be_bytes());
    header[4..8].copy_from_slice(&stream_id.to_be_bytes());
    let len = data.len() as u32;
    header[8..12].copy_from_slice(&len.to_be_bytes());

    stream.write_all(&header).await?;
    if !data.is_empty() {
        stream.write_all(data).await?;
    }
    stream.flush().await?;
    Ok(())
}

async fn read_raw_frame(stream: &mut CompatStream) -> io::Result<YamuxFrame> {
    let mut header = [0u8; 12];
    stream.read_exact(&mut header).await?;

    let frame_type = header[1];
    let flags = u16::from_be_bytes([header[2], header[3]]);
    let stream_id = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    let length = u32::from_be_bytes([header[8], header[9], header[10], header[11]]) as usize;

    let mut data = vec![0u8; length];
    if length > 0 {
        stream.read_exact(&mut data).await?;
    }

    Ok(YamuxFrame { stream_id, frame_type, flags, data })
}

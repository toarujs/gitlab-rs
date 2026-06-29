use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::net::{TcpStream, UnixStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};

pub struct SidechannelConnection {
    stream: Compat<CompatStream>,
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

const YAMUX_TYPE_DATA: u8 = 0;
const YAMUX_TYPE_WINDOW_UPDATE: u8 = 1;
const YAMUX_FLAG_SYN: u16 = 0x1;
const YAMUX_FLAG_ACK: u16 = 0x2;
const YAMUX_FLAG_FIN: u16 = 0x4;
const YAMUX_FLAG_RST: u16 = 0x8;

impl SidechannelConnection {
    pub async fn connect(addr: &str) -> io::Result<(String, Self)> {
        let mut key_bytes = [0u8; 32];
        let u1 = uuid::Uuid::new_v4();
        let u2 = uuid::Uuid::new_v4();
        key_bytes[..16].copy_from_slice(u1.as_bytes());
        key_bytes[16..].copy_from_slice(u2.as_bytes());
        let key_hex = hex::encode(&key_bytes);

        let mut stream = Self::connect_raw(addr).await?;

        stream.write_all(&key_bytes).await?;
        stream.flush().await?;

        let mut compat = stream.compat();

        send_frame(&mut compat, YAMUX_TYPE_DATA, YAMUX_FLAG_SYN, 0, &[]).await?;

        let frame = read_frame(&mut compat).await?;
        if frame.stream_id != 0 || frame.flags != (YAMUX_FLAG_SYN | YAMUX_FLAG_ACK) {
            return Err(io::Error::new(io::ErrorKind::Other,
                format!("unexpected yamux handshake response: type={}, flags={}, stream={}",
                    frame.type_, frame.flags, frame.stream_id)));
        }

        Ok((key_hex, Self { stream: compat }))
    }

    async fn connect_raw(addr: &str) -> io::Result<CompatStream> {
        if addr.starts_with("unix:") {
            let path = addr.trim_start_matches("unix:");
            Ok(CompatStream::Unix(UnixStream::connect(path).await?))
        } else {
            Ok(CompatStream::Tcp(TcpStream::connect(addr).await?))
        }
    }

    pub async fn accept(mut self) -> io::Result<SidechannelStream> {
        loop {
            let frame = read_frame(&mut self.stream).await?;

            if (frame.flags & YAMUX_FLAG_SYN) != 0
                && frame.stream_id > 0
                && frame.stream_id % 2 == 0
            {
                let stream_id = frame.stream_id;

                send_frame(&mut self.stream, YAMUX_TYPE_DATA, YAMUX_FLAG_SYN | YAMUX_FLAG_ACK, stream_id, &[]).await?;

                let window_delta: u32 = 0x4000_0000;
                send_frame(&mut self.stream, YAMUX_TYPE_WINDOW_UPDATE, 0, stream_id, &window_delta.to_be_bytes()).await?;

                return Ok(SidechannelStream {
                    stream: self.stream,
                    stream_id,
                    eof: false,
                });
            }
        }
    }
}

pub struct SidechannelStream {
    stream: Compat<CompatStream>,
    stream_id: u32,
    eof: bool,
}

impl SidechannelStream {
    pub async fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        send_frame(&mut self.stream, YAMUX_TYPE_DATA, 0, self.stream_id, data).await
    }

    pub async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let before = buf.len();
        loop {
            let frame = read_frame(&mut self.stream).await?;
            if frame.stream_id != self.stream_id {
                continue;
            }
            if (frame.flags & YAMUX_FLAG_RST) != 0 {
                return Err(io::Error::new(io::ErrorKind::ConnectionReset, "stream reset"));
            }
            if !frame.data.is_empty() {
                buf.extend_from_slice(&frame.data);
            }
            if (frame.flags & YAMUX_FLAG_FIN) != 0 {
                send_frame(&mut self.stream, YAMUX_TYPE_DATA, YAMUX_FLAG_FIN | YAMUX_FLAG_ACK, self.stream_id, &[]).await?;
                self.eof = true;
                return Ok(buf.len() - before);
            }
        }
    }

    pub async fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let mut offset = 0;
        while offset < buf.len() {
            let frame = read_frame(&mut self.stream).await?;
            if frame.stream_id != self.stream_id {
                continue;
            }
            if (frame.flags & YAMUX_FLAG_RST) != 0 {
                return Err(io::Error::new(io::ErrorKind::ConnectionReset, "stream reset"));
            }
            if !frame.data.is_empty() {
                let n = frame.data.len().min(buf.len() - offset);
                buf[offset..offset + n].copy_from_slice(&frame.data[..n]);
                offset += n;
            }
            if (frame.flags & YAMUX_FLAG_FIN) != 0 && offset < buf.len() {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "stream closed before read_exact completed"));
            }
        }
        Ok(())
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        if !self.eof {
            send_frame(&mut self.stream, YAMUX_TYPE_DATA, YAMUX_FLAG_FIN, self.stream_id, &[]).await?;
            self.eof = true;
        }
        Ok(())
    }
}

impl tokio::io::AsyncRead for SidechannelStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::new(io::ErrorKind::Other,
            "SidechannelStream does not support raw AsyncRead; use read_to_end or read_exact")))
    }
}

impl tokio::io::AsyncWrite for SidechannelStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(io::Error::new(io::ErrorKind::Other,
            "SidechannelStream does not support raw AsyncWrite; use write_all")))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

async fn send_frame(stream: &mut Compat<CompatStream>, type_: u8, flags: u16, stream_id: u32, data: &[u8]) -> io::Result<()> {
    let mut header = [0u8; 12];
    header[1] = type_;
    header[2..4].copy_from_slice(&flags.to_be_bytes());
    header[4..8].copy_from_slice(&stream_id.to_be_bytes());
    let len = data.len() as u32;
    header[8..12].copy_from_slice(&len.to_be_bytes());

    use futures::AsyncWriteExt;
    (&mut *stream).write_all(&header).await?;
    if !data.is_empty() {
        (&mut *stream).write_all(data).await?;
    }
    (&mut *stream).flush().await?;
    Ok(())
}

async fn read_frame(stream: &mut Compat<CompatStream>) -> io::Result<YamuxFrame> {
    use futures::AsyncReadExt;

    let mut header = [0u8; 12];
    (&mut *stream).read_exact(&mut header).await?;

    let type_ = header[1];
    let flags = u16::from_be_bytes([header[2], header[3]]);
    let stream_id = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    let length = u32::from_be_bytes([header[8], header[9], header[10], header[11]]) as usize;

    let mut data = vec![0u8; length];
    if length > 0 {
        (&mut *stream).read_exact(&mut data).await?;
    }

    Ok(YamuxFrame { type_, flags, stream_id, data })
}

struct YamuxFrame {
    type_: u8,
    flags: u16,
    stream_id: u32,
    data: Vec<u8>,
}

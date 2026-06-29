use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::net::{TcpStream, UnixStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use futures::future::poll_fn;

pub struct SidechannelConnection {
    connection: yamux::Connection<Compat<CompatStream>>,
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

        let compat = stream.compat();
        let connection = yamux::Connection::new(compat, yamux::Config::default(), yamux::Mode::Client);

        Ok((key_hex, Self { connection }))
    }

    async fn connect_raw(addr: &str) -> io::Result<CompatStream> {
        if addr.starts_with("unix:") {
            let path = addr.trim_start_matches("unix:");
            Ok(CompatStream::Unix(UnixStream::connect(path).await?))
        } else {
            Ok(CompatStream::Tcp(TcpStream::connect(addr).await?))
        }
    }

    pub async fn accept(&mut self) -> io::Result<SidechannelStream> {
        let conn = &mut self.connection;
        poll_fn(|cx| conn.poll_next_inbound(cx)).await
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "yamux connection closed"))?
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }
}

pub struct SidechannelStream {
    stream: yamux::Stream,
}

impl SidechannelStream {
    pub async fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        use futures::AsyncWriteExt;
        (&mut self.stream).write_all(data).await
    }

    pub async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        use futures::AsyncReadExt;
        let before = buf.len();
        (&mut self.stream).read_to_end(buf).await?;
        Ok(buf.len() - before)
    }

    pub async fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        use futures::AsyncReadExt;
        (&mut self.stream).read_exact(buf).await
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        use futures::AsyncWriteExt;
        (&mut self.stream).close().await
    }
}

impl tokio::io::AsyncRead for SidechannelStream {
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

impl tokio::io::AsyncWrite for SidechannelStream {
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

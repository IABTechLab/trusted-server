use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// An I/O stream that replays parser over-read bytes exactly once before the socket.
pub struct PrefixedIo<T> {
    inner: T,
    prefix: Vec<u8>,
    position: usize,
}

impl<T> PrefixedIo<T> {
    #[must_use]
    pub fn new(inner: T, prefix: Vec<u8>) -> Self {
        Self {
            inner,
            prefix,
            position: 0,
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for PrefixedIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.position < self.prefix.len() {
            let count = (self.prefix.len() - self.position).min(buf.remaining());
            let end = self.position + count;
            buf.put_slice(&self.prefix[self.position..end]);
            self.position = end;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for PrefixedIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, duplex};

    #[tokio::test]
    async fn prefix_is_delivered_once_before_inner_bytes() {
        let (mut writer, reader) = duplex(16);
        tokio::spawn(async move {
            writer.write_all(b"socket").await.expect("write inner");
        });
        let mut prefixed = PrefixedIo::new(reader, b"prefix-".to_vec());
        let mut output = Vec::new();
        prefixed.read_to_end(&mut output).await.expect("read all");
        assert_eq!(output, b"prefix-socket");
    }
}

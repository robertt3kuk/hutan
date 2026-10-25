//! `hutan proxy`: a bytes-only bridge between standard input/output and the local daemon's
//! socket (ADR 0022, slice R-2). `ssh host hutan proxy` makes the host's daemon reachable
//! through the SSH channel without any listener on the host.
//!
//! The bridge never decodes, buffers or reorders a frame: it copies what one side reads to the
//! other and flushes after every read, so latency is one read. Its memory is one
//! [`BUFFER`]-sized buffer per direction; backpressure is the pipe's, since a side that does
//! not read stops the other direction's write. It writes nothing to standard output of its own:
//! the SSH client treats every byte there as protocol.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Size of the buffer of each direction.
pub const BUFFER: usize = 64 * 1024;

/// Which side ended the bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// Standard input reached end of file: the client is gone or finished.
    Input,
    /// The daemon closed its socket.
    Daemon,
    /// Nobody reads standard output any more (the SSH channel closed).
    Output,
}

/// Copies `input` to `daemon` and `daemon` to `output` until either side closes, flushing after
/// every read. A write that finds its reader gone ends the bridge the same way a close does.
pub async fn bridge<I, O, D>(mut input: I, mut output: O, daemon: D) -> io::Result<Ended>
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
    D: AsyncRead + AsyncWrite + Unpin,
{
    let (mut daemon_read, mut daemon_write) = tokio::io::split(daemon);
    let upstream = async {
        let mut buffer = vec![0u8; BUFFER];
        loop {
            let n = input.read(&mut buffer).await?;
            if n == 0 {
                return Ok(Ended::Input);
            }
            daemon_write.write_all(&buffer[..n]).await?;
            daemon_write.flush().await?;
        }
    };
    let downstream = async {
        let mut buffer = vec![0u8; BUFFER];
        loop {
            let n = daemon_read.read(&mut buffer).await?;
            if n == 0 {
                return Ok(Ended::Daemon);
            }
            if let Err(e) = output.write_all(&buffer[..n]).await {
                return closed(e, Ended::Output);
            }
            if let Err(e) = output.flush().await {
                return closed(e, Ended::Output);
            }
        }
    };
    tokio::select! {
        ended = upstream => ended,
        ended = downstream => ended,
    }
}

/// `ended` for a peer that went away, the error for anything else.
fn closed(error: io::Error, ended: Ended) -> io::Result<Ended> {
    match error.kind() {
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset => Ok(ended),
        _ => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    use super::*;

    #[tokio::test]
    async fn bytes_cross_both_ways_and_input_eof_ends_the_bridge() {
        let (mut client, input) = duplex(64);
        let (output, mut client_out) = duplex(64);
        let (daemon, mut remote) = duplex(64);
        let task = tokio::spawn(bridge(input, output, daemon));

        client.write_all(b"hello").await.unwrap();
        let mut got = [0u8; 5];
        remote.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hello");

        remote.write_all(b"world!").await.unwrap();
        let mut got = [0u8; 6];
        client_out.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"world!");

        drop(client);
        assert_eq!(task.await.unwrap().unwrap(), Ended::Input);
    }

    #[tokio::test]
    async fn the_daemon_closing_ends_the_bridge() {
        let (_client, input) = duplex(64);
        let (output, _client_out) = duplex(64);
        let (daemon, remote) = duplex(64);
        let task = tokio::spawn(bridge(input, output, daemon));
        drop(remote);
        assert_eq!(task.await.unwrap().unwrap(), Ended::Daemon);
    }

    #[tokio::test]
    async fn a_reader_that_is_gone_ends_the_bridge() {
        let (_client, input) = duplex(64);
        let (output, client_out) = duplex(64);
        let (daemon, mut remote) = duplex(64);
        let task = tokio::spawn(bridge(input, output, daemon));
        drop(client_out);
        remote.write_all(b"x").await.unwrap();
        assert_eq!(task.await.unwrap().unwrap(), Ended::Output);
    }

    #[tokio::test]
    async fn a_full_pipe_holds_the_other_side_back_instead_of_growing() {
        // The reader never reads: the writer is held at the pipe's capacity, and one buffer
        // is all the bridge ever holds for it.
        let (mut client, input) = duplex(1024);
        let (output, _unread) = duplex(16);
        let (daemon, mut remote) = duplex(1 << 20);
        let task = tokio::spawn(bridge(input, output, daemon));
        remote.write_all(&[7u8; 4096]).await.unwrap();
        // Input still flows while output is stuck.
        client.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        remote.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");
        task.abort();
    }
}

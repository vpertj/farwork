//! Startup guard: a wildcard bind can coexist with a loopback listener on the
//! same port (SO_REUSEADDR on macOS/Linux), which silently leaves
//! 127.0.0.1:<port> traffic to the older process. Probe first so a lost port
//! fails loudly instead of producing a green-looking server that serves nobody.

use std::io::{Error, ErrorKind};
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};

const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// Bind `0.0.0.0:<port>`, unless something is already answering on it.
pub async fn bind(port: u16) -> std::io::Result<TcpListener> {
    if port_is_taken(port).await {
        return Err(Error::new(
            ErrorKind::AddrInUse,
            format!("port {port} is already in use"),
        ));
    }
    TcpListener::bind(("0.0.0.0", port)).await
}

async fn port_is_taken(port: u16) -> bool {
    // A refused connection means the port is free; a timeout is ambiguous but
    // treated as free so firewalled hosts still start.
    matches!(
        tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(("127.0.0.1", port))).await,
        Ok(Ok(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refuses_a_port_that_something_already_serves() {
        let occupied = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = occupied.local_addr().expect("addr").port();

        let error = bind(port).await.expect_err("occupied port must fail");
        assert_eq!(error.kind(), ErrorKind::AddrInUse);
        assert!(error.to_string().contains("already in use"));
        drop(occupied);
    }

    #[tokio::test]
    async fn binds_a_free_port() {
        let probe = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = probe.local_addr().expect("addr").port();
        drop(probe);

        let listener = bind(port).await.expect("free port binds");
        assert_eq!(listener.local_addr().expect("addr").port(), port);
    }
}

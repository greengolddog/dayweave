use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    time::timeout,
};

const DEFAULT_BIND_ADDRESS: &str = "0.0.0.0:8080";

/// Performs an HTTP liveness check against the loopback side of the configured
/// API listener.
///
/// The bind address is read from `DAYWEAVE_BIND_ADDRESS` or `DAYWEAVE_BIND`.
/// Unspecified bind IPs are replaced by the corresponding loopback IP.
///
/// # Errors
///
/// Returns [`HealthcheckError`] when the address is invalid, the request times
/// out, the connection fails, or `/health` does not return HTTP 200.
pub async fn local_healthcheck(timeout_duration: Duration) -> Result<(), HealthcheckError> {
    let bind_address = std::env::var("DAYWEAVE_BIND_ADDRESS")
        .or_else(|_| std::env::var("DAYWEAVE_BIND"))
        .unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_owned());
    let bind_address = bind_address
        .parse::<SocketAddr>()
        .map_err(|_| HealthcheckError::InvalidBindAddress(bind_address))?;
    check_address(loopback_for(bind_address), timeout_duration).await
}

/// Checks `/health` on an explicit socket address.
///
/// # Errors
///
/// Returns [`HealthcheckError`] when the request times out, the connection
/// fails, or the endpoint does not return HTTP 200.
pub async fn check_address(
    address: SocketAddr,
    timeout_duration: Duration,
) -> Result<(), HealthcheckError> {
    timeout(timeout_duration, async {
        let mut stream = TcpStream::connect(address).await?;
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut status_line = String::new();
        BufReader::new(stream).read_line(&mut status_line).await?;
        if status_line.starts_with("HTTP/1.1 200 ") || status_line.starts_with("HTTP/1.0 200 ") {
            Ok(())
        } else {
            Err(HealthcheckError::UnhealthyStatus(
                status_line.trim().to_owned(),
            ))
        }
    })
    .await
    .map_err(|_| HealthcheckError::TimedOut)?
}

fn loopback_for(address: SocketAddr) -> SocketAddr {
    let ip = match address.ip() {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
    };
    SocketAddr::new(ip, address.port())
}

#[derive(Debug, Error)]
pub enum HealthcheckError {
    #[error("invalid API bind address: {0}")]
    InvalidBindAddress(String),
    #[error("healthcheck timed out")]
    TimedOut,
    #[error("healthcheck I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("health endpoint returned an unhealthy status: {0}")]
    UnhealthyStatus(String),
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    #[test]
    fn maps_bound_interfaces_to_loopback() {
        assert_eq!(
            loopback_for("0.0.0.0:8787".parse().unwrap()),
            "127.0.0.1:8787".parse().unwrap()
        );
        assert_eq!(
            loopback_for("[::]:8787".parse().unwrap()),
            "[::1]:8787".parse().unwrap()
        );
    }

    #[tokio::test]
    async fn checks_http_status_not_only_the_tcp_port() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 256];
            let count = stream.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..count]).starts_with("GET /health "));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });

        check_address(address, Duration::from_secs(1))
            .await
            .expect("healthy endpoint");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_non_success_status() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 256];
            let count = stream.read(&mut request).await.unwrap();
            assert!(count > 0);
            stream
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });

        assert!(matches!(
            check_address(address, Duration::from_secs(1)).await,
            Err(HealthcheckError::UnhealthyStatus(_))
        ));
        server.await.unwrap();
    }
}

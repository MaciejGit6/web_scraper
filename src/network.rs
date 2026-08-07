use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const HTTP_PORT: u16 = 80;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn connect_to_web_server(domain: &[u8]) -> io::Result<TcpStream> {
    // Convert mmap bytes into a Rust string.
    let host = std::str::from_utf8(domain)
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "domain is not valid UTF-8",
            )
        })?
        .trim();

    if host.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "domain is empty",
        ));
    }

    // Resolve domain name -> IP address(es).
    let addresses = (host, HTTP_PORT).to_socket_addrs()?;

    let mut last_error = None;

    // A domain can resolve to several IPv4/IPv6 addresses.
    for address in addresses {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(stream) => {
                stream.set_read_timeout(Some(IO_TIMEOUT))?;
                stream.set_write_timeout(Some(IO_TIMEOUT))?;

                return Ok(stream);
            }

            Err(error) => {
                last_error = Some(error);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no addresses found for {host}"),
        )
    }))
}
#![allow(dead_code)]

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

// These fixtures accept one bounded HTTP/1 request per loopback connection.
// Tests choose when to answer each request, including concurrent range requests.
pub struct Server {
    listener: TcpListener,
    address: SocketAddr,
}

impl Server {
    pub fn bind() -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        Ok(Self { listener, address })
    }

    pub fn addr(&self) -> SocketAddr {
        self.address
    }

    pub fn recv_timeout(&self, timeout: Duration) -> io::Result<Option<Request>> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.listener.accept() {
                Ok((socket, _)) => return Request::read(socket, timeout).map(Some),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => return Err(error),
            }
        }
    }
}

pub struct Request {
    socket: TcpStream,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn read(socket: TcpStream, timeout: Duration) -> io::Result<Self> {
        socket.set_nonblocking(false)?;
        socket.set_read_timeout(Some(timeout))?;
        socket.set_write_timeout(Some(timeout))?;
        let mut reader = BufReader::new(socket);
        let mut raw = Vec::new();
        while !raw.ends_with(b"\r\n\r\n") {
            let remaining = 8192usize - raw.len();
            if remaining == 0 || (&mut reader).take(remaining as u64).read_until(b'\n', &mut raw)? == 0 {
                return Err(io::Error::other("incomplete fixture request headers"));
            }
        }
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut parsed = httparse::Request::new(&mut headers);
        let complete = parsed.parse(&raw).map_err(io::Error::other)?.is_complete();
        let path = parsed.path.filter(|_| complete)
            .ok_or_else(|| io::Error::other("invalid fixture request"))?.to_owned();
        let mut length = None;
        let mut headers = Vec::with_capacity(parsed.headers.len());
        for header in parsed.headers {
            let value = std::str::from_utf8(header.value).map_err(io::Error::other)?;
            if header.name.eq_ignore_ascii_case("Transfer-Encoding") {
                return Err(io::Error::other("fixture expects a fixed-length request"));
            }
            if header.name.eq_ignore_ascii_case("Content-Length") {
                let size = value.parse::<usize>().map_err(io::Error::other)?;
                if length.replace(size).is_some() || size > 1024 * 1024 {
                    return Err(io::Error::other("invalid fixture request length"));
                }
            }
            headers.push((header.name.to_owned(), value.to_owned()));
        }
        let mut body = vec![0; length.unwrap_or(0)];
        reader.read_exact(&mut body)?;
        Ok(Self { socket: reader.into_inner(), path, headers, body })
    }

    pub fn into_stream(self) -> TcpStream {
        self.socket
    }

    pub fn url(&self) -> &str {
        &self.path
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub fn reply(self, body: impl AsRef<[u8]>) -> io::Result<()> {
        self.send("200 OK", &[], body.as_ref(), true)
    }

    pub fn reply_range(self, range: &str, body: &[u8]) -> io::Result<()> {
        self.send("206 Partial Content", &[("Content-Range", range)], body, true)
    }

    pub fn reply_until_close(self, body: &[u8]) -> io::Result<()> {
        self.send("200 OK", &[], body, false)
    }

    fn send(mut self, status: &str, headers: &[(&str, &str)], body: &[u8], fixed_length: bool) -> io::Result<()> {
        write!(self.socket, "HTTP/1.1 {status}\r\nConnection: close\r\n")?;
        for (name, value) in headers {
            write!(self.socket, "{name}: {value}\r\n")?;
        }
        if fixed_length {
            write!(self.socket, "Content-Length: {}\r\n", body.len())?;
        }
        self.socket.write_all(b"\r\n")?;
        self.socket.write_all(body)
    }
}

use super::*;
use std::{net::TcpListener, thread};

fn serve_once(
    response: impl FnOnce(&mut BufReader<TcpStream>) + Send + 'static,
) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut stream = BufReader::new(socket);
        let mut request = String::new();
        loop {
            let before = request.len();
            assert!(stream.read_line(&mut request).unwrap() > 0);
            if &request[before..] == "\r\n" {
                break;
            }
        }
        assert!(request.starts_with("POST /api/runtime/private-model/install HTTP/1.1\r\n"));
        assert!(request.contains("\r\nExpect: 100-continue\r\n"));
        assert!(request.contains("\r\nContent-Length: 6\r\n"));
        assert!(request.contains("\r\nx-respire-model-id: model.v1\r\n"));
        response(&mut stream);
    });
    (port, server)
}

fn write_reply(wire: &mut BufReader<TcpStream>, status: &str, media: &str, body: &str) {
    write!(wire.get_mut(), "HTTP/1.1 {status}\r\nContent-Type: {media}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
}

struct NoUpload;
impl Read for NoUpload {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("the private model helper read upload input before receiving 100 Continue");
    }
}

#[test]
fn returns_early_problem_without_consuming_input_or_retrying() {
    let (port, server) = serve_once(|wire| {
        write_reply(
            wire,
            "409 Conflict",
            "application/problem+json",
            r#"{"status":409,"code":"managed_model_unavailable"}"#,
        );
    });
    let (status, body) = install(port, "rhm1_fixture", "model.v1", 6, &mut NoUpload).unwrap();
    assert_eq!(status, 409);
    assert_eq!(body["code"], "managed_model_unavailable");
    server.join().unwrap();
}

#[test]
fn uploads_exact_payload_after_one_continue_and_gets_final_receipt() {
    let (port, server) = serve_once(|wire| {
        wire.get_mut()
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .unwrap();
        let mut data = [0u8; 6];
        wire.read_exact(&mut data).unwrap();
        assert_eq!(&data, b"model!");
        write_reply(
            wire,
            "201 Created",
            "application/json",
            r#"{"outcome":"installed"}"#,
        );
    });
    let mut source = b"model!extra".as_slice();
    let (status, body) = install(port, "rhm1_fixture", "model.v1", 6, &mut source).unwrap();
    assert_eq!(status, 201);
    assert_eq!(body["outcome"], "installed");
    assert_eq!(source, b"extra");
    server.join().unwrap();
}

#[test]
fn refuses_redirect_success_before_upload_and_uncertain_framing() {
    for response in [
        "HTTP/1.1 307 Redirect\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\n\r\n",
        "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
        "HTTP/1.1 409 Conflict\r\nContent-Type: application/problem+json\r\nContent-Length: 9\r\nContent-Length: 9\r\n\r\n",
        "HTTP/1.1 409 Conflict\r\nContent-Type: application/problem+json\r\nContent-Length: 9\r\nTransfer-Encoding: chunked\r\n\r\n",
        "HTTP/1.1 409 Conflict\r\nContent-Type: application/problem+json\r\nTransfer-Encoding: chunked\r\n\r\n",
        "HTTP/1.1 409 Conflict\r\nContent-Type: application/problem+json\r\nContent-Length: 4097\r\n\r\n",
        "HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 100 Continue\r\n\r\n",
        "HTTP/1.1 409 Conflict\r\nContent-Type: application/problem+json\r\nContent-Length: 16\r\n\r\n{\"status\":500}\r\n",
        "HTTP/1.1 409 Conflict\r\nContent-Type: application/problem+json\r\nContent-Length: 40\r\n\r\n{\"status\":409}",
    ] {
        let (port, server) = serve_once(move |wire| {
            wire.get_mut().write_all(response.as_bytes()).unwrap();
        });
        assert!(install(port, "rhm1_fixture", "model.v1", 6, &mut NoUpload).is_err());
        server.join().unwrap();
    }
}

#[test]
fn rejects_oversized_headers_without_reading_input() {
    let reply = format!(
        "HTTP/1.1 409 Conflict\r\nX-Padding: {}\r\n\r\n",
        "a".repeat(HEADER_BYTES)
    );
    let (port, server) = serve_once(move |wire| {
        let _ = wire.get_mut().write_all(reply.as_bytes());
    });
    assert!(install(port, "rhm1_fixture", "model.v1", 6, &mut NoUpload).is_err());
    server.join().unwrap();
}

#[test]
fn distinguishes_post_upload_error_from_missing_final_receipt() {
    for (reply, expected) in [("409", Some(409)), ("", None)] {
        let (port, server) = serve_once(move |wire| {
            wire.get_mut()
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .unwrap();
            let mut data = [0u8; 6];
            wire.read_exact(&mut data).unwrap();
            if reply == "409" {
                write_reply(
                    wire,
                    "409 Conflict",
                    "application/problem+json",
                    r#"{"status":409,"code":"managed_model_unavailable"}"#,
                );
            }
        });
        let observed = install(
            port,
            "rhm1_fixture",
            "model.v1",
            6,
            &mut b"model!".as_slice(),
        );
        assert_eq!(observed.ok().map(|r| r.0), expected);
        server.join().unwrap();
    }
}

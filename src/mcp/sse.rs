use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::error::{Error, Result};

use super::protocol::{Request, Response, PARSE_ERROR};
use super::Handler;

/// Interval between keep-alive comments, below the timeout of common intermediaries.
const KEEPALIVE: Duration = Duration::from_secs(15);

/// Longest a client may take to send its request, and longest a write may block.
///
/// Without these a client that opens a connection and sends one byte holds a thread for
/// as long as the process lives, and a few thousand such clients exhaust it.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Largest request body accepted.
///
/// A `Content-Length` header used to be believed and allocated up front, so
/// `Content-Length: 99999999999` aborted the process on allocation failure.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Caps on the request head, for the same reason.
const MAX_HEADER_COUNT: usize = 100;
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// Connections served at once. Beyond this the listener answers 503 rather than spawning
/// an unbounded number of threads.
const MAX_CONCURRENT_CONNECTIONS: usize = 64;

type Sessions = Arc<Mutex<HashMap<String, Sender<String>>>>;

/// Lock helper that survives a poisoned mutex.
///
/// The scope below never joins, so a panic in a connection thread is not re-raised until
/// the process ends — which meant one panic poisoned the session registry and every
/// later request panicked in turn, a one-shot denial of service. Every other lock in this
/// crate already recovers this way.
fn sessions_lock(
    sessions: &Sessions,
) -> std::sync::MutexGuard<'_, HashMap<String, Sender<String>>> {
    match sessions.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Serve over HTTP with server-sent events, on the loopback interface unless told otherwise.
pub fn serve(handler: &Handler, bind: &str) -> Result<()> {
    let address = bind
        .to_socket_addrs()
        .map_err(|e| Error::Mcp(format!("could not resolve `{bind}`: {e}")))?
        .next()
        .ok_or_else(|| Error::Mcp(format!("`{bind}` resolved to no address")))?;

    if !address.ip().is_loopback() {
        eprintln!(
            "mahiron-ctx: serving on {address}, which is reachable from other machines; \
             anything that can reach it can read any file this process can read"
        );
    }

    let listener = TcpListener::bind(address)
        .map_err(|e| Error::Mcp(format!("could not listen on {address}: {e}")))?;
    eprintln!("mahiron-ctx: MCP over SSE on http://{address}/sse");

    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
    let live = Arc::new(AtomicUsize::new(0));

    std::thread::scope(|scope| {
        for incoming in listener.incoming() {
            let stream = match incoming {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
            let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));

            if live.fetch_add(1, Ordering::AcqRel) >= MAX_CONCURRENT_CONNECTIONS {
                live.fetch_sub(1, Ordering::AcqRel);
                let _ = respond(stream, "503 Service Unavailable", "text/plain", b"busy");
                continue;
            }

            let sessions = Arc::clone(&sessions);
            let live = Arc::clone(&live);
            scope.spawn(move || {
                let _ = handle_connection(stream, handler, &sessions);
                live.fetch_sub(1, Ordering::AcqRel);
            });
        }
    });
    Ok(())
}

/// Everything the request head carries that the handler needs.
struct Head {
    method: String,
    target: String,
    origin: Option<String>,
    host: Option<String>,
    content_length: usize,
}

/// Whether a request may act on this server.
///
/// The specification requires a local MCP server to validate `Origin`, precisely because
/// any page in the user's browser can reach `127.0.0.1`. Nothing here used to be checked,
/// and the session identifier was optional, so there was no token to get wrong either: a
/// simple cross-origin form post reached the tool call. The response is unreadable to the
/// attacker thanks to CORS, but the side effects are the point — the request names the
/// paths to read and the path to write.
fn origin_is_permitted(head: &Head) -> bool {
    // A browser always sends `Origin` on a cross-origin request; a local agent generally
    // sends none at all. An absent origin is therefore not a browser and is allowed.
    if let Some(origin) = &head.origin {
        if !host_is_loopback(
            origin
                .trim_start_matches("http://")
                .trim_start_matches("https://"),
        ) {
            return false;
        }
    }
    // `Host` is what a DNS-rebinding attack controls: the socket is loopback but the name
    // resolved to it is the attacker's, which is what lets the browser treat it as
    // same-origin later in the attack.
    match &head.host {
        Some(host) => host_is_loopback(host),
        None => true,
    }
}

fn host_is_loopback(value: &str) -> bool {
    let authority = value.split('/').next().unwrap_or(value);
    let host = match authority.rsplit_once(':') {
        // Not a port if what follows is not a number; IPv6 literals contain colons.
        Some((head, port)) if port.chars().all(|c| c.is_ascii_digit()) => head,
        _ => authority,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn read_head(reader: &mut BufReader<TcpStream>) -> Result<Option<Head>> {
    let mut request_line = String::new();
    if read_line_bounded(reader, &mut request_line)? == 0 {
        return Ok(None);
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();

    let mut head = Head {
        method,
        target,
        origin: None,
        host: None,
        content_length: 0,
    };

    for _ in 0..MAX_HEADER_COUNT {
        let mut header = String::new();
        if read_line_bounded(reader, &mut header)? == 0 || header.trim().is_empty() {
            return Ok(Some(head));
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            // A length that will not parse is a malformed request, not an empty body.
            head.content_length = value
                .parse()
                .map_err(|_| Error::Mcp(format!("unreadable Content-Length `{value}`")))?;
        } else if name.eq_ignore_ascii_case("origin") {
            head.origin = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("host") {
            head.host = Some(value.to_string());
        }
    }
    Err(Error::Mcp("too many request headers".into()))
}

/// Read one line, refusing a line long enough to be an attack on memory.
fn read_line_bounded(reader: &mut BufReader<TcpStream>, out: &mut String) -> Result<usize> {
    let mut taken = reader.take(MAX_HEADER_BYTES as u64);
    let read = taken.read_line(out)?;
    if read >= MAX_HEADER_BYTES {
        return Err(Error::Mcp("request header line is too long".into()));
    }
    Ok(read)
}

fn handle_connection(stream: TcpStream, handler: &Handler, sessions: &Sessions) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let head = match read_head(&mut reader) {
        Ok(Some(head)) => head,
        Ok(None) => return Ok(()),
        Err(_) => {
            return respond(
                stream,
                "400 Bad Request",
                "text/plain",
                b"malformed request",
            )
        }
    };

    if !origin_is_permitted(&head) {
        return respond(
            stream,
            "403 Forbidden",
            "text/plain",
            b"this server answers only same-machine callers",
        );
    }

    if head.content_length > MAX_BODY_BYTES {
        return respond(
            stream,
            "413 Payload Too Large",
            "text/plain",
            b"request body is too large",
        );
    }

    let (path, query) = head
        .target
        .split_once('?')
        .unwrap_or((head.target.as_str(), ""));

    match (head.method.as_str(), path) {
        ("GET", "/sse") => open_event_stream(stream, sessions),
        ("POST", "/message") | ("POST", "/messages") => {
            let mut body = vec![0u8; head.content_length];
            reader.read_exact(&mut body)?;
            accept_message(stream, &body, query, handler, sessions)
        }
        _ => respond(stream, "404 Not Found", "text/plain", b"not found"),
    }
}

fn open_event_stream(mut stream: TcpStream, sessions: &Sessions) -> Result<()> {
    let session = new_session_id()?;
    let (sender, receiver): (Sender<String>, Receiver<String>) = channel();
    sessions_lock(sessions).insert(session.clone(), sender);

    stream.write_all(
        b"HTTP/1.1 200 OK\r\n\
          Content-Type: text/event-stream\r\n\
          Cache-Control: no-store\r\n\
          Connection: keep-alive\r\n\r\n",
    )?;
    write!(
        stream,
        "event: endpoint\ndata: /message?sessionId={session}\n\n"
    )?;
    stream.flush()?;

    loop {
        match receiver.recv_timeout(KEEPALIVE) {
            Ok(message) => {
                if write!(stream, "event: message\ndata: {message}\n\n").is_err()
                    || stream.flush().is_err()
                {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if stream.write_all(b": keep-alive\n\n").is_err() || stream.flush().is_err() {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    sessions_lock(sessions).remove(&session);
    Ok(())
}

fn accept_message(
    stream: TcpStream,
    body: &[u8],
    query: &str,
    handler: &Handler,
    sessions: &Sessions,
) -> Result<()> {
    let session = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == "sessionId")
        .map(|(_, value)| value.to_string());

    // A message must name a live event stream. This used to be optional, which meant the
    // identifier was not a token at all and any caller could invoke a tool without ever
    // having opened one.
    let Some(session) = session else {
        return respond(
            stream,
            "400 Bad Request",
            "text/plain",
            b"a message must name the session it belongs to",
        );
    };
    if !sessions_lock(sessions).contains_key(&session) {
        return respond(
            stream,
            "404 Not Found",
            "text/plain",
            b"no such session; open /sse first",
        );
    }

    let response = match serde_json::from_slice::<Request>(body) {
        Ok(request) => handler.handle(request),
        Err(error) => Some(Response::error(
            Value::Null,
            PARSE_ERROR,
            format!("could not read the request: {error}"),
        )),
    };

    if let Some(response) = response {
        let encoded = serde_json::to_string(&response).map_err(|e| Error::Mcp(e.to_string()))?;
        let delivered = sessions_lock(sessions)
            .get(&session)
            .map(|sender| sender.send(encoded.clone()).is_ok())
            .unwrap_or(false);

        // The stream went away between the check above and now; the response goes back on
        // this request rather than being dropped.
        if !delivered {
            return respond(stream, "200 OK", "application/json", encoded.as_bytes());
        }
    }
    respond(stream, "202 Accepted", "text/plain", b"")
}

fn respond(mut stream: TcpStream, status: &str, content_type: &str, body: &[u8]) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

/// A session identifier that cannot be guessed.
///
/// It was the process id and a nanosecond timestamp, which is guessable to within the
/// clock's resolution. It is the token that decides whose event stream a response is
/// delivered on, so it has to be a secret.
fn new_session_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| Error::Mcp(format!("no source of randomness for a session id: {e}")))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(origin: Option<&str>, host: Option<&str>) -> Head {
        Head {
            method: "POST".into(),
            target: "/message".into(),
            origin: origin.map(str::to_string),
            host: host.map(str::to_string),
            content_length: 0,
        }
    }

    #[test]
    fn a_page_on_another_origin_is_refused() {
        assert!(!origin_is_permitted(&head(
            Some("https://evil.example"),
            Some("127.0.0.1:7681")
        )));
    }

    #[test]
    fn a_rebound_host_name_is_refused() {
        // The socket is loopback, but the name the browser used is not.
        assert!(!origin_is_permitted(&head(
            None,
            Some("attacker.example:7681")
        )));
    }

    #[test]
    fn a_local_agent_and_a_local_page_are_allowed() {
        assert!(origin_is_permitted(&head(None, Some("127.0.0.1:7681"))));
        assert!(origin_is_permitted(&head(
            Some("http://localhost:7681"),
            Some("localhost:7681")
        )));
        assert!(origin_is_permitted(&head(None, None)));
        assert!(origin_is_permitted(&head(None, Some("[::1]:7681"))));
    }

    #[test]
    fn session_identifiers_are_unpredictable() {
        let first = new_session_id().unwrap();
        let second = new_session_id().unwrap();
        assert_ne!(first, second);
        assert_eq!(first.len(), 32);
    }
}

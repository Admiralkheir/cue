//! A tiny HTTP server for provider tests: replies with canned responses, in order,
//! and records what it received.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

pub struct Reply {
    status: u16,
    content_type: &'static str,
    body: String,
}

impl Reply {
    pub fn new(status: u16, content_type: &'static str, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type,
            body: body.into(),
        }
    }

    pub fn json(status: u16, body: &str) -> Self {
        Self::new(status, "application/json", body)
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub struct MockServer {
    pub url: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl MockServer {
    pub fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();

        thread::spawn(move || {
            for reply in replies {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream);
                recorded.lock().unwrap().push(read_request(&mut reader));

                let response = format!(
                    "HTTP/1.1 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    reply.status,
                    reply.content_type,
                    reply.body.len(),
                    reply.body
                );
                let _ = reader.get_mut().write_all(response.as_bytes());
            }
        });

        Self { url, requests }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

fn read_request(reader: &mut impl BufRead) -> Request {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();

    let mut headers = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((key, value)) = header.split_once(':') {
            headers.push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }

    let length = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();

    Request {
        path,
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}

/// A streamed response with one server-sent event per payload. Payloads may span lines
/// for readability; each is sent on one.
pub fn sse(events: &[&str]) -> Reply {
    let body: String = events
        .iter()
        .map(|data| format!("event: x\ndata: {}\n\n", data.replace('\n', " ")))
        .collect();
    Reply::new(200, "text/event-stream", body)
}

/// A search tool that answers every query the same way and records the queries.
pub struct FakeSearch {
    results: String,
    queries: Mutex<Vec<String>>,
}

impl FakeSearch {
    pub fn new(results: &str) -> Self {
        Self {
            results: results.into(),
            queries: Mutex::default(),
        }
    }

    pub fn queries(&self) -> Vec<String> {
        self.queries.lock().unwrap().clone()
    }
}

impl super::WebSearch for FakeSearch {
    fn search(&self, query: &str) -> Result<String, String> {
        self.queries.lock().unwrap().push(query.to_owned());
        Ok(self.results.clone())
    }
}

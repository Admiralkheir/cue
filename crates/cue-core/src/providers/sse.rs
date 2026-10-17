//! Minimal Server-Sent Events reader for streaming responses.

use std::io::{self, BufRead};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub event: Option<String>,
    pub data: String,
}

pub struct Events<R> {
    reader: R,
    line: String,
}

impl<R: BufRead> Events<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            line: String::new(),
        }
    }
}

impl<R: BufRead> Iterator for Events<R> {
    type Item = io::Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut event = None;
        let mut data: Option<String> = None;
        loop {
            self.line.clear();
            match self.reader.read_line(&mut self.line) {
                Ok(0) => return data.map(|data| Ok(Event { event, data })),
                Ok(_) => {}
                Err(err) => return Some(Err(err)),
            }
            let line = self.line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                // A blank line ends the event; skip blank lines between events.
                match data.take() {
                    Some(data) => return Some(Ok(Event { event, data })),
                    None => continue,
                }
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => event = Some(value.to_owned()),
                "data" => match &mut data {
                    Some(data) => {
                        data.push('\n');
                        data.push_str(value);
                    }
                    None => data = Some(value.to_owned()),
                },
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(input: &str) -> Vec<Event> {
        Events::new(input.as_bytes()).map(Result::unwrap).collect()
    }

    #[test]
    fn parses_events_comments_and_multiline_data() {
        let input = ": ping\n\nevent: a\ndata: {\"x\":1}\n\ndata: one\ndata: two\r\n\r\nevent: b\ndata:tail";
        assert_eq!(
            events(input),
            [
                Event {
                    event: Some("a".into()),
                    data: "{\"x\":1}".into()
                },
                Event {
                    event: None,
                    data: "one\ntwo".into()
                },
                Event {
                    event: Some("b".into()),
                    data: "tail".into()
                },
            ]
        );
    }
}

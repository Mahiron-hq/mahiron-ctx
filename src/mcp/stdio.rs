use std::io::{BufRead, BufReader, Write};

use serde_json::Value;

use crate::error::Result;

use super::protocol::{Request, Response, PARSE_ERROR};
use super::Handler;

/// Serve over standard input and output, one JSON-RPC message per line.
///
/// The document itself never travels on this channel except as a tool result, so nothing
/// may be printed to standard output that is not a response.
pub fn serve(handler: &Handler) -> Result<()> {
    let input = BufReader::new(std::io::stdin());
    let stdout = std::io::stdout();

    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => handler.handle(request),
            Err(error) => Some(Response::error(
                Value::Null,
                PARSE_ERROR,
                format!("could not read the request: {error}"),
            )),
        };

        if let Some(response) = response {
            let mut locked = stdout.lock();
            serde_json::to_writer(&mut locked, &response)
                .map_err(|e| crate::Error::Mcp(e.to_string()))?;
            locked.write_all(b"\n")?;
            locked.flush()?;
        }
    }
    Ok(())
}

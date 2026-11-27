use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;
use tayassu_core::ProviderKind;

/// Maximum time for the entire quota exchange.
const DEADLINE: Duration = Duration::from_secs(10);
/// Maximum accepted JSON-RPC frame size.
const MAX_FRAME: usize = 1024 * 1024;
const INITIALIZE_ID: u64 = 1;
const QUOTA_ID: u64 = 2;

pub(crate) fn read_quota(mut command: Command, provider: ProviderKind) -> Result<Value> {
    let framed = provider == ProviderKind::Copilot;
    if framed {
        command.args(["--server", "--stdio"]);
    } else {
        command.args(["app-server", "--listen", "stdio://"]);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start quota server")?;
    let result = (|| {
        let mut input = child
            .stdin
            .take()
            .context("quota server stdin unavailable")?;
        let output = child
            .stdout
            .take()
            .context("quota server stdout unavailable")?;
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                match read_frame(&mut reader, framed) {
                    Ok(value) if value.get("id").is_none() => {}
                    Ok(value) => {
                        let done = value["id"] == QUOTA_ID;
                        if sender.send(Ok(value)).is_err() || done {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });

        let start = std::time::Instant::now();
        if !framed {
            send(
                &mut input,
                &json!({"id": INITIALIZE_ID, "method": "initialize",
                "params": {"clientInfo": {"name": "tayassu", "version": env!("CARGO_PKG_VERSION")}}}),
                framed,
            )?;
            receive(&receiver, INITIALIZE_ID, start)?;
            send(&mut input, &json!({"method": "initialized"}), framed)?;
        }
        let method = if framed {
            "account.getQuota"
        } else {
            "account/rateLimits/read"
        };
        let request = if framed {
            json!({"jsonrpc": "2.0", "id": QUOTA_ID, "method": method, "params": {}})
        } else {
            json!({"id": QUOTA_ID, "method": method})
        };
        send(&mut input, &request, framed)?;
        receive(&receiver, QUOTA_ID, start)
    })();
    let _ = child.kill();
    child.wait().context("failed to reap quota server")?;
    result
}

fn send(input: &mut impl Write, value: &Value, framed: bool) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    if framed {
        write!(input, "Content-Length: {}\r\n\r\n", body.len())?;
    }
    input.write_all(&body)?;
    if !framed {
        input.write_all(b"\n")?;
    }
    input.flush()?;
    Ok(())
}

fn receive(
    receiver: &Receiver<Result<Value>>,
    id: u64,
    start: std::time::Instant,
) -> Result<Value> {
    loop {
        let reply = receiver
            .recv_timeout(DEADLINE.saturating_sub(start.elapsed()))
            .context("quota server did not respond")??;
        if reply["id"] == id {
            if reply.get("error").is_some() {
                bail!("quota RPC request failed");
            }
            return reply
                .get("result")
                .cloned()
                .context("quota RPC result missing");
        }
    }
}

fn read_frame(reader: &mut impl BufRead, framed: bool) -> Result<Value> {
    if !framed {
        let mut line = Vec::new();
        if reader
            .take(MAX_FRAME as u64 + 1)
            .read_until(b'\n', &mut line)?
            == 0
            || line.len() > MAX_FRAME
        {
            bail!("quota server closed stdout or exceeded the frame limit");
        }
        return serde_json::from_slice(&line).context("invalid quota JSON");
    }
    let mut header = String::new();
    reader.take(MAX_FRAME as u64).read_line(&mut header)?;
    let length = header
        .trim()
        .strip_prefix("Content-Length: ")
        .context("invalid quota RPC header")?
        .parse::<usize>()?;
    header.clear();
    reader.take(MAX_FRAME as u64).read_line(&mut header)?;
    if header != "\r\n" || length > MAX_FRAME {
        bail!("invalid quota RPC frame");
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).context("invalid quota JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_framed_json_rpc_responses() {
        let expected = json!({"jsonrpc": "2.0", "id": QUOTA_ID, "result": {"quotaSnapshots": {}}});
        let mut wire = Vec::new();
        send(&mut wire, &expected, true).expect("write frame");
        let decoded = read_frame(&mut Cursor::new(wire), true).expect("read frame");
        assert_eq!(decoded, expected);
    }
}

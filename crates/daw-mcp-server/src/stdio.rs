//! Newline-delimited JSON-RPC over stdin and stdout: each request handled as it arrives, answers written whole, in
//! the order they finish.
use crate::host::Host;
use crate::mcp;
use daw_bridge::frame::Framer;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

/// The longest message read from the client.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Serves until the input ends (or breaks), then lets the requests in flight answer.
pub async fn serve<I, O>(host: Arc<Host>, mut input: I, mut output: O) -> std::io::Result<()>
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin + Send + 'static,
{
    let (answers, mut answered) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(async move {
        while let Some(line) = answered.recv().await {
            output.write_all(&line).await?;
            output.flush().await?;
        }
        Ok::<_, std::io::Error>(())
    });
    // The bridge's events, as notifications between the answers.
    let mut events = host.events();
    let notices = answers.clone();
    let forwarding = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let notification = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/daw_event", "params": event});
                    if notices.send(encode(&notification)).is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let mut framer = Framer::new(MAX_MESSAGE_BYTES);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut tasks = tokio::task::JoinSet::new();
    'reading: loop {
        while let Some(line) = framer.next_line() {
            let Ok(line) = line else {
                // A message too long or not UTF-8: nothing after it can be framed reliably.
                let _ =
                    answers.send(encode(&serde_json::json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}})));
                break 'reading;
            };
            let host = host.clone();
            let answers = answers.clone();
            tasks.spawn(async move {
                let answer = match serde_json::from_str::<Value>(&line) {
                    Ok(message) => mcp::handle(&host, &message).await,
                    Err(_) => Some(serde_json::json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}})),
                };
                if let Some(answer) = answer {
                    let _ = answers.send(encode(&answer));
                }
            });
        }
        match input.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(count) => framer.feed(&chunk[..count]),
        }
    }
    while tasks.join_next().await.is_some() {}
    forwarding.abort();
    drop(answers);
    writer.await.unwrap_or(Ok(()))?;
    host.close().await;
    Ok(())
}

fn encode(value: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(value).expect("JSON serializes");
    line.push(b'\n');
    line
}

use std::time::Duration;
use anyhow::{anyhow, bail, Context};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::timeout;
use tracing::trace;

const TARGET: &str = "qmp";
const QMP_TIMEOUT: Duration = Duration::from_secs(10);

/// Executes one QMP command on the VM whose QMP server listens on 127.0.0.1:`port`,
/// and returns the content of its `return` field.
pub async fn qmp_execute(port: u16, command: &str, arguments: Option<Value>) -> anyhow::Result<Value> {
    timeout(QMP_TIMEOUT, qmp_execute_inner(port, command, arguments))
        .await
        .map_err(|_| anyhow!("QMP command \"{command}\" on port {port} timed out"))?
}

async fn qmp_execute_inner(port: u16, command: &str, arguments: Option<Value>) -> anyhow::Result<Value> {
    let stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .with_context(|| format!("Could not connect to QMP on port {port}"))?;

    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    // Greeting
    loop {
        let line = lines.next_line().await?.ok_or_else(|| anyhow!("QMP connection closed before greeting"))?;
        let message: Value = serde_json::from_str(&line)?;

        if message.get("QMP").is_some() {
            break;
        }
    }

    send(&mut writer, json!({ "execute": "qmp_capabilities" })).await?;
    read_return(&mut lines).await?;

    let mut message = json!({ "execute": command });
    if let Some(arguments) = arguments {
        message["arguments"] = arguments;
    }

    send(&mut writer, message).await?;
    read_return(&mut lines).await
}

async fn send(writer: &mut OwnedWriteHalf, message: Value) -> anyhow::Result<()> {
    trace!(target: TARGET, "> {}", message);
    writer.write_all(format!("{message}\n").as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_return(lines: &mut Lines<BufReader<OwnedReadHalf>>) -> anyhow::Result<Value> {
    while let Some(line) = lines.next_line().await? {
        trace!(target: TARGET, "< {}", line);
        let message: Value = serde_json::from_str(&line)?;

        if let Some(result) = message.get("return") {
            return Ok(result.clone());
        }

        if let Some(error) = message.get("error") {
            bail!("QMP error: {}", error);
        }

        // Asynchronous events are ignored
    }

    bail!("QMP connection closed before a response was received")
}

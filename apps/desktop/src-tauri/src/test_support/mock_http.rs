#![allow(dead_code)]

use tokio::io::{AsyncWrite, AsyncWriteExt};

pub const CONNECTION_CLOSE: &str = "Connection: close";

pub async fn write_head<W>(
    sock: &mut W,
    status: &str,
    headers: &[(&str, &str)],
    content_length: Option<usize>,
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut head = String::from("HTTP/1.1 ");
    head.push_str(status);
    head.push_str("\r\n");
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("connection") {
            continue;
        }
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    if let Some(length) = content_length {
        head.push_str("Content-Length: ");
        head.push_str(&length.to_string());
        head.push_str("\r\n");
    }
    head.push_str(CONNECTION_CLOSE);
    head.push_str("\r\n\r\n");
    sock.write_all(head.as_bytes()).await?;
    sock.flush().await
}

pub async fn write_response<W>(
    sock: &mut W,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    write_head(
        sock,
        status,
        &[("Content-Type", content_type)],
        Some(body.len()),
    )
    .await?;
    sock.write_all(body).await?;
    sock.flush().await
}

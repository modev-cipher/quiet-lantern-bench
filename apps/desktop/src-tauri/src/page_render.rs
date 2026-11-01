use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::sidecar::client::{post_sse, SseEvent};
use crate::storage::{book, cache, new_id};

pub const PAGE_PNG_KIND: &str = "page_png";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderPagesRequest {
    pub book_path: String,
    pub content_hash: String,
    pub pages: Vec<i64>,
    pub cache_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    pub with_geometry: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct PageRender {
    pub page_no: i64,
    pub hash: String,
    pub bytes: i64,
    pub cached: bool,
    #[serde(default)]
    pub geometry_version: Option<String>,
    #[serde(default)]
    pub geometry: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RenderEvent {
    Progress { page: i64, total: i64 },
    Done { pages: Vec<PageRender> },
}

#[derive(Debug)]
enum Outcome {
    Result(Vec<PageRender>),
    Cancelled,
    Error { code: String, message: String },
}

async fn engine_health_field(
    client: &reqwest::Client,
    port: u16,
    token: &str,
    field: &str,
) -> Option<String> {
    let url = format!("http://127.0.0.1:{port}/health");
    let response = client
        .get(url)
        .bearer_auth(token)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: serde_json::Value = response.json().await.ok()?;
    body[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub async fn engine_geometry_version(
    client: &reqwest::Client,
    port: u16,
    token: &str,
) -> Option<String> {
    engine_health_field(client, port, token, "geometry_version").await
}

/// The extraction identity this build would stamp on a freshly ingested book.
///
/// Read from the engine rather than duplicated in Rust: the desktop shipped two authorities
/// for the raster scale once already, and a stamp that disagrees with the thing it
/// describes reports fresh for a generation it does not describe (§2.4f).
pub async fn engine_extract_version(
    client: &reqwest::Client,
    port: u16,
    token: &str,
) -> Option<String> {
    engine_health_field(client, port, token, "extract_version").await
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenerationFence {
    pub blocks_version: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct PersistTarget<'a> {
    pub book_id: &'a str,
    pub fence: Option<&'a GenerationFence>,
}

impl<'a> PersistTarget<'a> {
    pub fn unfenced(book_id: &'a str) -> Self {
        Self {
            book_id,
            fence: None,
        }
    }

    pub fn fenced(book_id: &'a str, fence: &'a GenerationFence) -> Self {
        Self {
            book_id,
            fence: Some(fence),
        }
    }
}

pub async fn render_pages<F>(
    pool: &SqlitePool,
    client: &reqwest::Client,
    port: u16,
    token: &str,
    book_id: &str,
    request: &RenderPagesRequest,
    on_event: F,
) -> Result<Vec<PageRender>, String>
where
    F: FnMut(RenderEvent),
{
    render_pages_fenced(
        pool,
        client,
        port,
        token,
        PersistTarget::unfenced(book_id),
        request,
        on_event,
    )
    .await
}

pub async fn render_pages_fenced<F>(
    pool: &SqlitePool,
    client: &reqwest::Client,
    port: u16,
    token: &str,
    target: PersistTarget<'_>,
    request: &RenderPagesRequest,
    mut on_event: F,
) -> Result<Vec<PageRender>, String>
where
    F: FnMut(RenderEvent),
{
    let book_id = target.book_id;
    let job_id = new_id();
    let mut outcome: Option<Outcome> = None;
    post_sse(
        client,
        port,
        token,
        "/render_pages",
        &job_id,
        request,
        |event: SseEvent| match event.event.as_str() {
            "progress" => {
                let page = event.data["page"].as_i64().unwrap_or(0);
                let total = event.data["total"].as_i64().unwrap_or(0);
                on_event(RenderEvent::Progress { page, total });
            }
            "result" => {
                let skipped_pages: Vec<i64> =
                    serde_json::from_value(event.data["skipped"].clone()).unwrap_or_default();
                if !skipped_pages.is_empty() {
                    tracing::warn!(
                        "renderer skipped {} pages: {:?}",
                        skipped_pages.len(),
                        skipped_pages
                    );
                }
                match serde_json::from_value::<Vec<PageRender>>(event.data["pages"].clone()) {
                    Ok(pages) => outcome = Some(Outcome::Result(pages)),
                    Err(error) => {
                        outcome = Some(Outcome::Error {
                            code: "ERR_DESERIALIZE".into(),
                            message: format!("failed to parse result pages: {error}"),
                        })
                    }
                }
            }
            "cancelled" => outcome = Some(Outcome::Cancelled),
            "error" => {
                outcome = Some(Outcome::Error {
                    code: event.data["code"].as_str().unwrap_or("ERR_INTERNAL").into(),
                    message: event.data["message"].as_str().unwrap_or("").into(),
                });
            }
            _ => {}
        },
    )
    .await
    .map_err(|error| error.to_string())?;

    let pages = match outcome {
        Some(Outcome::Result(pages)) => pages,
        Some(Outcome::Cancelled) => return Err("render cancelled".into()),
        Some(Outcome::Error { code, message }) => return Err(format!("{code}: {message}")),
        None => return Err("sidecar stream ended without a result".into()),
    };

    for page in &pages {
        cache::upsert_entry(pool, &page.hash, PAGE_PNG_KIND, page.bytes)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(row) = book::get_page(pool, book_id, page.page_no)
            .await
            .map_err(|e| e.to_string())?
        {
            let mut written = true;
            if let (Some(version), Some(geometry)) = (&page.geometry_version, &page.geometry) {
                let record = book::PageGeometry {
                    page_id: row.id.clone(),
                    geometry_version: version.clone(),
                    geometry_json: geometry.to_string(),
                };
                written = match target.fence {
                    Some(fence) => book::upsert_page_geometry_fenced(
                        pool,
                        &record,
                        book_id,
                        fence.blocks_version.as_deref(),
                    )
                    .await
                    .map_err(|e| e.to_string())?,
                    None => {
                        book::upsert_page_geometry(pool, &record)
                            .await
                            .map_err(|e| e.to_string())?;
                        true
                    }
                };
                if !written {
                    tracing::debug!(
                        "{}: book={} page={} outcome=generation-moved",
                        crate::reading::GEOMETRY_LOG_PREFIX,
                        book_id.chars().take(8).collect::<String>(),
                        page.page_no,
                    );
                }
            } else if let Some(fence) = target.fence {
                let current_version = book::get_book(pool, book_id)
                    .await
                    .map_err(|e| e.to_string())?
                    .and_then(|b| b.blocks_version);
                if current_version.as_deref() != fence.blocks_version.as_deref() {
                    written = false;
                }
            }
            if written {
                book::set_page_render_hash(pool, &row.id, &page.hash)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    on_event(RenderEvent::Done {
        pages: pages.clone(),
    });
    Ok(pages)
}

#[cfg(test)]
mod health_field_tests {
    use super::*;

    async fn spawn_health(body: &'static str, status: &'static str) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::AsyncReadExt;
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let _ = crate::test_support::mock_http::write_response(
                    &mut sock,
                    status,
                    "application/json",
                    body.as_bytes(),
                )
                .await;
            }
        });
        port
    }

    const HEALTH: &str = r#"{"status":"ok","geometry_version":"v1|geom=1|scale=3","extract_version":"v1|extract=3|roles=1"}"#;

    #[tokio::test]
    async fn each_reader_takes_its_own_field_from_the_one_health_body() {
        let port = spawn_health(HEALTH, "200 OK").await;
        let client = reqwest::Client::new();

        assert_eq!(
            engine_extract_version(&client, port, "tok")
                .await
                .as_deref(),
            Some("v1|extract=3|roles=1")
        );
        assert_eq!(
            engine_geometry_version(&client, port, "tok")
                .await
                .as_deref(),
            Some("v1|geom=1|scale=3"),
            "both readers share one helper, so a field name swapped in either would send the \
             other's stamp to a comparison that would then always agree"
        );
    }

    #[tokio::test]
    async fn a_missing_or_empty_field_reads_as_no_answer_rather_than_a_verdict() {
        let port = spawn_health(r#"{"status":"ok","extract_version":""}"#, "200 OK").await;
        let client = reqwest::Client::new();

        assert_eq!(engine_extract_version(&client, port, "tok").await, None);
        assert_eq!(engine_geometry_version(&client, port, "tok").await, None);
    }

    #[tokio::test]
    async fn an_unhealthy_engine_is_no_answer_and_never_a_stale_verdict() {
        let port = spawn_health(HEALTH, "503 Service Unavailable").await;
        let client = reqwest::Client::new();

        let answer = engine_extract_version(&client, port, "tok").await;
        assert_eq!(answer, None);
        assert!(
            crate::library::generation_is_current(Some("v1|extract=1|roles=1"), answer.as_deref()),
            "a down engine must not cost the user a multi-minute pass"
        );
    }
}

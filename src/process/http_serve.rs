use anyhow::Result;
use axum::{body::Body, http::StatusCode, response::Response};
use std::{net::SocketAddr, path::Path as StdPath, path::PathBuf};
use tokio_util::io::ReaderStream;
use tracing::info;

#[allow(dead_code)]
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut idx = 0;
    while value >= 1024.0 && idx < UNITS.len() - 1 {
        value /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.2} {}", value, UNITS[idx])
    }
}

#[allow(dead_code)]
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

#[allow(dead_code)]
fn mime_for(path: &StdPath) -> mime::Mime {
    mime_guess::from_path(path).first_or_octet_stream()
}

#[allow(dead_code)]
async fn file_response(path: &StdPath) -> Result<Response, ServeError> {
    let meta = tokio::fs::metadata(path).await?;
    let len = meta.len();
    let file = tokio::fs::File::open(path).await?;
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);
    let resp = Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, mime_for(path).to_string())
        .header(axum::http::header::CONTENT_LENGTH, len)
        .body(body)
        .map_err(|e| ServeError::Io(std::io::Error::other(e)))?;
    Ok(resp)
}

#[derive(Debug)]
#[allow(dead_code)]
enum ServeError {
    NotFound,
    Forbidden,
    BadRequest,
    Io(std::io::Error),
}

impl From<std::io::Error> for ServeError {
    fn from(e: std::io::Error) -> Self {
        ServeError::Io(e)
    }
}

impl axum::response::IntoResponse for ServeError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        let (status, msg) = match self {
            ServeError::NotFound => {
                tracing::warn!("ServeError: NotFound");
                (StatusCode::NOT_FOUND, "Not Found")
            }
            ServeError::Forbidden => {
                tracing::warn!("ServeError: Forbidden");
                (StatusCode::FORBIDDEN, "Forbidden")
            }
            ServeError::BadRequest => {
                tracing::warn!("ServeError: BadRequest");
                (StatusCode::BAD_REQUEST, "Bad Request")
            }
            ServeError::Io(ref e) => {
                tracing::error!("ServeError: Io({})", e);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
            }
        };
        (status, msg).into_response()
    }
}

#[allow(dead_code)]
fn safe_join(root: &StdPath, rel: &str) -> Result<PathBuf, ServeError> {
    let canonical_root = root.canonicalize().map_err(|_| ServeError::NotFound)?;
    if rel.is_empty() || rel == "." {
        return Ok(canonical_root);
    }
    // Track depth relative to canonical root to catch ".." traversal.
    // This is necessary because PathBuf::components() resolves ".." away, so
    // a naive starts_with check can pass for "/parent/child" when the root
    // is "/parent/child/grandchild" (since "/parent/child" starts with "/parent").
    let mut depth: isize = 0;
    for seg in rel.split('/') {
        if seg.is_empty() {
            continue;
        } else if seg == "." {
            // no-op
        } else if seg == ".." {
            depth -= 1;
            if depth < 0 {
                return Err(ServeError::Forbidden);
            }
        } else {
            depth += 1;
        }
    }
    // After processing, if depth is still 0 or negative, the path tried to
    // escape above the root and should have been rejected above.
    // Now construct the candidate by joining to canonical_root.
    let candidate = canonical_root.join(rel);
    let normalized: PathBuf = candidate.components().collect();
    if !normalized.starts_with(&canonical_root) {
        return Err(ServeError::Forbidden);
    }
    // Canonicalize to resolve symlinks. After canonicalization, re-check that
    // the resolved path is still inside the canonical root. This catches
    // symlink escapes (e.g. root/sub -> /etc, requesting /sub -> /etc).
    let final_path = normalized
        .canonicalize()
        .map_err(|_| ServeError::NotFound)?;
    if !final_path.starts_with(&canonical_root) {
        return Err(ServeError::Forbidden);
    }
    Ok(final_path)
}

async fn serve_root_handler(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<HttpServeState>>,
) -> Result<axum::response::Response, ServeError> {
    let abs = safe_join(&state.path, "")?;
    dir_handler(state, abs).await
}

async fn serve_handler(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<HttpServeState>>,
    axum::extract::Path(path): axum::extract::Path<String>,
) -> Result<axum::response::Response, ServeError> {
    let abs = safe_join(&state.path, &path)?;
    let meta = tokio::fs::symlink_metadata(&abs)
        .await
        .map_err(|_| ServeError::NotFound)?;
    if meta.is_dir() {
        dir_handler(state, abs).await
    } else {
        tracing::info!("Streaming {:?} ({} bytes)", abs, meta.len());
        Ok(file_response(&abs).await?)
    }
}

async fn dir_handler(
    state: std::sync::Arc<HttpServeState>,
    abs: std::path::PathBuf,
) -> Result<axum::response::Response, ServeError> {
    let index = abs.join("index.html");
    if tokio::fs::try_exists(&index).await.unwrap_or(false) {
        if let Ok(meta) = tokio::fs::metadata(&index).await {
            if meta.is_file() {
                tracing::info!("Serving {:?} (index.html, {} bytes)", abs, meta.len());
                return file_response(&index).await;
            }
        }
    }
    let html = render_listing(&state.path, &abs).await?;
    let resp = axum::response::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(axum::body::Body::from(html))
        .map_err(|e| ServeError::Io(std::io::Error::other(e)))?;
    Ok(resp)
}

fn render_breadcrumb(root: &StdPath, current: &StdPath) -> String {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        "<a href=\"/\">{}</a>",
        html_escape(
            &canonical_root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".to_string())
        )
    ));
    if let Ok(rel) = current.strip_prefix(&canonical_root) {
        let mut accum = String::new();
        for comp in rel.components() {
            let seg = comp.as_os_str().to_string_lossy();
            accum.push('/');
            accum.push_str(&seg);
            parts.push(format!(
                "<a href=\"{}\">{}</a>",
                html_escape(&accum),
                html_escape(&seg)
            ));
        }
    }
    parts.join(" / ")
}

#[derive(Debug)]
struct ListingEntry {
    name: String,
    is_dir: bool,
    size: Option<u64>,
    mtime: Option<std::time::SystemTime>,
}

async fn collect_entries(dir: &StdPath) -> Result<Vec<ListingEntry>, ServeError> {
    let mut out = Vec::new();
    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let meta = match tokio::fs::symlink_metadata(entry.path()).await {
            Ok(m) => m,
            Err(_) => {
                out.push(ListingEntry {
                    name,
                    is_dir: false,
                    size: None,
                    mtime: None,
                });
                continue;
            }
        };
        let is_dir = meta.is_dir();
        let size = if is_dir { None } else { Some(meta.len()) };
        let mtime = meta.modified().ok();
        out.push(ListingEntry {
            name,
            is_dir,
            size,
            mtime,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn render_entry_row(parent_href: &str, e: &ListingEntry) -> String {
    let href = format!(
        "{}/{}{}",
        parent_href.trim_end_matches('/'),
        html_escape(&e.name),
        if e.is_dir { "/" } else { "" }
    );
    let size = match (e.is_dir, e.size) {
        (true, _) => "—".to_string(),
        (false, Some(s)) => human_size(s),
        (false, None) => "—".to_string(),
    };
    let mtime = e
        .mtime
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| {
            let secs = d.as_secs() as i64;
            let dt = chrono::DateTime::from_timestamp(secs, 0);
            dt.map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                .unwrap_or_else(|| "—".to_string())
        })
        .unwrap_or_else(|| "—".to_string());
    format!(
        "<tr><td><a href=\"{}\">{}</a>{}</td><td>{}</td><td>{}</td></tr>",
        href,
        html_escape(&e.name),
        if e.is_dir { "/" } else { "" },
        size,
        mtime,
    )
}

#[allow(dead_code)]
async fn render_listing(root: &StdPath, current: &StdPath) -> Result<String, ServeError> {
    let entries = collect_entries(current).await?;
    let breadcrumb = render_breadcrumb(root, current);
    let parent_link = if current != root.canonicalize().unwrap_or_else(|_| root.to_path_buf()) {
        let parent = current
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| root.to_path_buf());
        let rel = parent
            .strip_prefix(root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
            .ok()
            .and_then(|p| p.to_str())
            .unwrap_or("");
        let href = if rel.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", rel)
        };
        format!("<p><a href=\"{}\">..</a></p>", href)
    } else {
        String::new()
    };
    let rows: String = entries.iter().map(|e| render_entry_row("/", e)).collect();
    let html = format!(
        "<!DOCTYPE html>\n\
<html><head><meta charset=\"utf-8\"><title>Index of {title}</title>\n\
<style>body{{font-family:sans-serif;max-width:60rem;margin:2rem auto;padding:0 1rem}}\
table{{border-collapse:collapse;width:100%}}\
th,td{{padding:.4rem .6rem;border-bottom:1px solid #ddd;text-align:left}}\
a{{color:#06c;text-decoration:none}}a:hover{{text-decoration:underline}}</style>\n\
</head><body>\n\
<h1>{breadcrumb}</h1>\n\
{parent}\
<table><thead><tr><th>Name</th><th>Size</th><th>Modified</th></tr></thead>\n\
<tbody>{rows}</tbody></table>\n\
</body></html>\n",
        title = html_escape(&current.to_string_lossy()),
        breadcrumb = breadcrumb,
        parent = parent_link,
        rows = rows,
    );
    Ok(html)
}

#[derive(Debug)]
struct HttpServeState {
    path: PathBuf,
}

pub async fn process_http_serve(path: PathBuf, port: u16) -> Result<()> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Serving {:?} on {}", path, listener.local_addr()?);
    serve(listener, path).await
}

pub async fn serve(listener: tokio::net::TcpListener, path: PathBuf) -> Result<()> {
    let state = HttpServeState { path };
    let router = axum::Router::new()
        .route("/", axum::routing::get(serve_root_handler))
        .route("/*path", axum::routing::get(serve_handler))
        .with_state(std::sync::Arc::new(state));
    axum::serve(listener, router).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mime::{APPLICATION_OCTET_STREAM, APPLICATION_PDF, IMAGE_JPEG, IMAGE_PNG, TEXT_HTML};
    use std::path::Path;

    #[tokio::test]
    async fn test_serve_handler_root_lists_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("a.txt"), b"x").unwrap();
        let state = std::sync::Arc::new(HttpServeState { path: root.clone() });
        let resp = serve_handler(
            axum::extract::State(state),
            axum::extract::Path("".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 16 * 1024)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("a.txt"));
    }

    #[tokio::test]
    async fn test_serve_handler_traversal_forbidden() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let state = std::sync::Arc::new(HttpServeState { path: root });
        let resp = serve_handler(
            axum::extract::State(state),
            axum::extract::Path("../outside".to_string()),
        )
        .await;
        match resp {
            Err(ServeError::Forbidden) => {}
            other => panic!("expected Forbidden, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_serve_handler_index_html_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("index.html"), b"<h1>INDEX</h1>").unwrap();
        std::fs::write(root.join("other.txt"), b"x").unwrap();
        let state = std::sync::Arc::new(HttpServeState { path: root });
        let resp = serve_handler(
            axum::extract::State(state),
            axum::extract::Path("".to_string()),
        )
        .await
        .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 16 * 1024)
            .await
            .unwrap();
        assert_eq!(&body[..], b"<h1>INDEX</h1>");
    }

    #[test]
    fn test_html_escape() {
        assert_eq!(html_escape("plain"), "plain");
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape("a & b"), "a &amp; b");
        assert_eq!(html_escape("\"quoted\""), "&quot;quoted&quot;");
        assert_eq!(html_escape("it's"), "it&#39;s");
        assert_eq!(html_escape("a<>\"'&b"), "a&lt;&gt;&quot;&#39;&amp;b");
    }

    #[test]
    fn test_human_size() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.00 KiB");
        assert_eq!(human_size(1536), "1.50 KiB");
        assert_eq!(human_size(1024 * 1024), "1.00 MiB");
        assert_eq!(human_size(1024u64.pow(3)), "1.00 GiB");
        assert_eq!(human_size(1024u64.pow(4)), "1.00 TiB");
        assert_eq!(human_size(1024u64.pow(5)), "1.00 PiB");
        assert_eq!(human_size(1024u64.pow(6)), "1024.00 PiB");
    }

    #[test]
    fn test_mime_for() {
        assert_eq!(mime_for(Path::new("a.html")), TEXT_HTML);
        assert_eq!(mime_for(Path::new("a.png")), IMAGE_PNG);
        assert_eq!(mime_for(Path::new("a.jpg")), IMAGE_JPEG);
        assert_eq!(mime_for(Path::new("a.pdf")), APPLICATION_PDF);
        assert_eq!(mime_for(Path::new("a.unknown")), APPLICATION_OCTET_STREAM);
        assert_eq!(mime_for(Path::new("a")), APPLICATION_OCTET_STREAM);
    }

    #[test]
    fn test_safe_join_basic() {
        let root = PathBuf::from(".");
        let p = safe_join(&root, "Cargo.toml").expect("file exists");
        assert!(p.ends_with("Cargo.toml"));
        assert_eq!(safe_join(&root, "").unwrap(), root.canonicalize().unwrap());
        assert_eq!(safe_join(&root, ".").unwrap(), root.canonicalize().unwrap());
    }

    #[test]
    fn test_safe_join_traversal() {
        let root = PathBuf::from(".");
        match safe_join(&root, "../outside-target") {
            Err(ServeError::Forbidden) => {}
            other => panic!("expected Forbidden, got {:?}", other),
        }
        match safe_join(&root, "src/../../../etc/passwd") {
            Err(ServeError::Forbidden) => {}
            other => panic!("expected Forbidden, got {:?}", other),
        }
        match safe_join(&root, "does-not-exist") {
            Err(ServeError::NotFound) => {}
            other => panic!("expected NotFound, got {:?}", other),
        }
        // sanity: a real subdir still resolves
        assert!(safe_join(&root, "src").is_ok());
    }

    #[test]
    fn test_render_breadcrumb_root() {
        let root = PathBuf::from(".").canonicalize().unwrap();
        let html = render_breadcrumb(&root, &root);
        assert!(html.contains(&format!(
            ">{}<",
            root.file_name().unwrap().to_string_lossy()
        )));
        assert!(!html.contains(".."));
    }

    #[test]
    fn test_render_breadcrumb_subdir() {
        let root = PathBuf::from(".").canonicalize().unwrap();
        let sub = safe_join(&root, "src").unwrap();
        let html = render_breadcrumb(&root, &sub);
        assert!(html.contains("src"));
        assert!(html.contains(&root.file_name().unwrap().to_string_lossy().to_string()));
    }

    #[tokio::test]
    async fn test_render_listing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("alpha.txt"), b"hi").unwrap();
        std::fs::write(root.join("beta.txt"), b"there").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join(".hidden"), b"nope").unwrap();
        std::fs::write(root.join("index.html"), b"<h1>idx</h1>").unwrap();

        let html = render_listing(&root, &root).await.unwrap();

        assert!(html.contains("alpha.txt"));
        assert!(html.contains("beta.txt"));
        assert!(html.contains("sub/"));
        assert!(html.contains("index.html"));
        assert!(!html.contains(".hidden"));
        // alpha.txt link must not end with '/'
        assert!(html.contains("href=\"/alpha.txt\""));
        // subdir link must end with '/'
        assert!(html.contains("href=\"/sub/\""));
        // size column header present
        assert!(html.contains("Size"));
    }

    #[tokio::test]
    async fn test_file_response_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hello.txt");
        std::fs::write(&p, b"hello world").unwrap();
        let resp = file_response(&p).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "text/plain"
        );
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_LENGTH)
                .unwrap(),
            "11"
        );
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"hello world");
    }

    #[tokio::test]
    async fn test_file_response_binary() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("blob.bin");
        let payload: Vec<u8> = (0u8..=255).cycle().take(1024).collect();
        std::fs::write(&p, &payload).unwrap();
        let resp = file_response(&p).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "application/octet-stream"
        );
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_LENGTH)
                .unwrap(),
            "1024"
        );
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        assert_eq!(body.len(), 1024);
        assert_eq!(&body[..], &payload[..]);
    }

    #[tokio::test]
    async fn test_file_response_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("missing.txt");
        match file_response(&p).await {
            Err(ServeError::Io(_)) => {}
            other => panic!("expected Io error, got {:?}", other),
        }
    }
}

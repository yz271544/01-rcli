use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Router,
};
use std::{net::SocketAddr, path::Path as StdPath, path::PathBuf, sync::Arc};
use tower_http::services::ServeDir;
use tracing::{info, warn};

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

#[allow(dead_code)]
fn safe_join(root: &StdPath, rel: &str) -> Result<PathBuf, ServeError> {
    let candidate = if rel.is_empty() || rel == "." {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let canonical_root = root.canonicalize().map_err(|_| ServeError::NotFound)?;
    // Try canonicalizing the candidate first (handles existing paths, resolves symlinks).
    if let Ok(canonical_candidate) = candidate.canonicalize() {
        if !canonical_candidate.starts_with(&canonical_root) {
            return Err(ServeError::Forbidden);
        }
        return Ok(canonical_candidate);
    }
    // Candidate does not exist. Normalize both paths by collecting components
    // (resolves ".." and "." without requiring the target to exist).
    let normalized: PathBuf = candidate.components().collect();
    let normalized_root: PathBuf = root.components().collect();
    if !normalized.starts_with(&normalized_root) {
        return Err(ServeError::Forbidden);
    }
    // Reject any ".." in the relative portion after the root.
    let rel_part = normalized.strip_prefix(&normalized_root).unwrap();
    for component in rel_part.components() {
        if component == std::path::Component::ParentDir {
            return Err(ServeError::Forbidden);
        }
    }
    // Path is confirmed safe; canonicalize to resolve symlinks and return the final path.
    normalized.canonicalize().map_err(|_| ServeError::NotFound)
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
    let href = if e.is_dir {
        format!(
            "{}/{}/",
            parent_href.trim_end_matches('/'),
            html_escape(&e.name)
        )
    } else {
        format!(
            "{}/{}",
            parent_href.trim_end_matches('/'),
            html_escape(&e.name)
        )
    };
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
    info!("Serving {:?} on {}", path, addr);

    let state = HttpServeState { path: path.clone() };
    // axum router
    let router = Router::new()
        .nest_service("/tower", ServeDir::new(path))
        .route("/*path", get(file_handler))
        .with_state(Arc::new(state));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;
    Ok(())
}

async fn file_handler(
    State(state): State<Arc<HttpServeState>>,
    Path(path): Path<String>,
) -> (StatusCode, String) {
    let p = std::path::Path::new(&state.path).join(path);
    info!("Reading file {:?}", p);
    if !p.exists() {
        (
            StatusCode::NOT_FOUND,
            format!("File {} note found", p.display()),
        )
    } else {
        // TODO: test p is a directory
        // if it is a directory, list all files/subdirectories
        // as <li><a href="/path/to/file">file name</a></li>
        // <html><body><ul>...</ul></body></html>
        match tokio::fs::read_to_string(p).await {
            Ok(content) => {
                info!("Read {} bytes", content.len());
                (StatusCode::OK, content)
            }
            Err(e) => {
                warn!("Error reading file: {:?}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mime::{APPLICATION_OCTET_STREAM, APPLICATION_PDF, IMAGE_JPEG, IMAGE_PNG, TEXT_HTML};
    use std::path::Path;

    #[tokio::test]
    async fn test_file_handler() {
        let state = Arc::new(HttpServeState {
            path: PathBuf::from("."),
        });
        let (status, content) = file_handler(State(state), Path("Cargo.toml".to_string())).await;
        assert_eq!(status, StatusCode::OK);
        assert!(content.trim().starts_with("[package]"));
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
}

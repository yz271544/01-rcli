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
}

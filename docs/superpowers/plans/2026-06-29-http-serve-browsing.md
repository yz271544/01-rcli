# http serve — directory browsing & binary download Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `rcli http serve` so directory requests render an HTML listing (with `index.html` precedence), and binary files stream correctly with the right `Content-Type`.

**Architecture:** Single file `src/process/http_serve.rs` is reorganised into small helper units (`safe_join`, `mime_for`, `html_escape`, `human_size`, `render_listing`, `file_response`, `serve_handler`). One router-level handler dispatches file vs directory based on `symlink_metadata`. Directory handler probes `index.html` first; otherwise renders a listing page assembled from inlined HTML strings (no template engine).

**Tech Stack:** Rust 2021, axum 0.7, tokio, tokio-util (new), mime_guess (new, promoted from transitive), tower-http (existing), reqwest (dev), tempfile (dev).

**Spec:** `docs/superpowers/specs/2026-06-29-http-serve-browsing-design.md`

---

## File Structure

| File                                       | Change  | Responsibility |
| ------------------------------------------ | ------- | -------------- |
| `Cargo.toml`                               | modify  | add `tokio-util`, `mime_guess`; add dev-deps `reqwest`, `tempfile` |
| `src/process/http_serve.rs`                | rewrite | routing, dispatcher, all helpers, unit tests |
| `tests/http_serve.rs`                      | create  | integration tests via `reqwest` against a port-0 listener |

`http_serve.rs` is a single file because the existing module is small (~75 lines) and the new code fits comfortably with section-comment dividers. No new modules are introduced.

---

## Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Edit `Cargo.toml`**

Add the following lines under `[dependencies]`:

```toml
mime_guess = "2"
tokio-util = { version = "0.7", features = ["io"] }
```

Add the following lines under a new `[dev-dependencies]` section at the bottom of `Cargo.toml` (after `[dependencies]` and any existing `[dev-dependencies]`):

```toml
[dev-dependencies]
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
tempfile = "3"
```

The final `[dependencies]` block should read (existing entries unchanged):

```toml
[dependencies]
anyhow = "1.0.81"
axum = { version = "0.7.4", features = ["http2", "query", "tracing"] }
base64 = "0.22.0"
blake3 = "1.5.1"
clap = { version = "4.5.3", features = ["derive"] }
csv = "1.3.0"
ed25519-dalek = { version = "2.1.1", features = ["rand_core"] }
enum_dispatch = "0.3.12"
mime_guess = "2"
rand = "0.8.5"
serde = { version = "1.0.197", features = ["derive"] }
serde_json = "1.0.114"
serde_yaml = "0.9.33"
tokio = { version = "1.36.0", features = ["rt", "rt-multi-thread", "macros", "net", "fs"] }
tokio-util = { version = "0.7", features = ["io"] }
tower-http = { version = "0.5.2", features = ["compression-full", "cors", "trace", "fs"] }
tracing = "0.1.40"
tracing-subscriber = { version = "0.3.18", features = ["env-filter"] }
zxcvbn = "2.2.2"
```

- [ ] **Step 2: Verify deps resolve and the project still compiles**

Run: `cargo check --tests`
Expected: compiles with no errors. Warnings about unused imports in `src/process/http_serve.rs` (because the file is about to be rewritten) are acceptable for this task — fix them in Task 8.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add tokio-util, mime_guess deps and reqwest, tempfile dev-deps"
```

---

## Task 2: `html_escape` helper

**Files:**
- Modify: `src/process/http_serve.rs` (add module-private function and unit test)

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` block at the bottom of `src/process/http_serve.rs`:

```rust
    #[test]
    fn test_html_escape() {
        assert_eq!(html_escape("plain"), "plain");
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape("a & b"), "a &amp; b");
        assert_eq!(html_escape("\"quoted\""), "&quot;quoted&quot;");
        assert_eq!(html_escape("it's"), "it&#39;s");
        assert_eq!(html_escape("a<>\"'&b"), "a&lt;&gt;&quot;&#39;&amp;b");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib test_html_escape`
Expected: FAIL with compile error `cannot find function html_escape`.

- [ ] **Step 3: Implement `html_escape`**

Add the following to `src/process/http_serve.rs` near the top (after the `use` statements):

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib test_html_escape`
Expected: PASS, 1 passed.

- [ ] **Step 5: Commit**

```bash
git add src/process/http_serve.rs
git commit -m "feat(http-serve): add html_escape helper"
```

---

## Task 3: `human_size` helper

**Files:**
- Modify: `src/process/http_serve.rs`

- [ ] **Step 1: Write the failing test**

Append to `#[cfg(test)] mod tests`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib test_human_size`
Expected: FAIL with compile error `cannot find function human_size`.

- [ ] **Step 3: Implement `human_size`**

Add to `src/process/http_serve.rs`:

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib test_human_size`
Expected: PASS, 1 passed.

- [ ] **Step 5: Commit**

```bash
git add src/process/http_serve.rs
git commit -m "feat(http-serve): add human_size helper"
```

---

## Task 4: `mime_for` helper

**Files:**
- Modify: `src/process/http_serve.rs`

- [ ] **Step 1: Write the failing test**

Append to `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn test_mime_for() {
        assert_eq!(mime_for(Path::new("a.html")), "text/html");
        assert_eq!(mime_for(Path::new("a.png")), "image/png");
        assert_eq!(mime_for(Path::new("a.jpg")), "image/jpeg");
        assert_eq!(mime_for(Path::new("a.pdf")), "application/pdf");
        assert_eq!(mime_for(Path::new("a.unknown")), "application/octet-stream");
        assert_eq!(mime_for(Path::new("a")), "application/octet-stream");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib test_mime_for`
Expected: FAIL with compile error `cannot find function mime_for`.

- [ ] **Step 3: Implement `mime_for`**

Add the imports at the top of `src/process/http_serve.rs` (after the existing `use` block):

```rust
use std::borrow::Cow;
```

Add the helper:

```rust
fn mime_for(path: &Path) -> mime::Mime {
    mime_guess::from_path(path).first_or_octet_stream()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib test_mime_for`
Expected: PASS, 1 passed.

Note: `mime::Mime`'s `PartialEq` compares type/subtype only, so `assert_eq!` against the string `"text/html"` works after `mime_for` is deref'd through `.to_string()` — but here `mime_for` returns `mime::Mime` directly. To make the assertions compile, adjust the test to compare against `mime::Mime` literals. Replace the test body with:

```rust
    #[test]
    fn test_mime_for() {
        assert_eq!(mime_for(Path::new("a.html")), mime::TEXT_HTML);
        assert_eq!(mime_for(Path::new("a.png")), mime::IMAGE_PNG);
        assert_eq!(mime_for(Path::new("a.jpg")), mime::IMAGE_JPEG);
        assert_eq!(mime_for(Path::new("a.pdf")), mime::APPLICATION_PDF);
        assert_eq!(mime_for(Path::new("a.unknown")), mime::APPLICATION_OCTET_STREAM);
        assert_eq!(mime_for(Path::new("a")), mime::APPLICATION_OCTET_STREAM);
    }
```

Re-run `cargo test --lib test_mime_for`. Expected: PASS, 1 passed.

- [ ] **Step 5: Commit**

```bash
git add src/process/http_serve.rs
git commit -m "feat(http-serve): add mime_for helper"
```

---

## Task 5: `safe_join` helper

**Files:**
- Modify: `src/process/http_serve.rs`

- [ ] **Step 1: Add `ServeError` type**

Above the `safe_join` declaration (we'll add it next), add the error enum:

```rust
#[derive(Debug)]
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
```

(We will wire `IntoResponse` later, in Task 8, after axum types are imported.)

- [ ] **Step 2: Write the failing test**

Append to `#[cfg(test)] mod tests`:

```rust
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
        let canonical_root = root.canonicalize().unwrap();
        // create a temp dir outside of root to ensure the escape attempt is real
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().to_path_buf();

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
        // silence unused-variable warnings
        let _ = (canonical_root, outside_path);
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib test_safe_join`
Expected: FAIL with compile error `cannot find function safe_join`.

- [ ] **Step 4: Implement `safe_join`**

Add to `src/process/http_serve.rs`:

```rust
fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, ServeError> {
    let candidate = if rel.is_empty() || rel == "." {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let canonical_root = root.canonicalize().map_err(|_| ServeError::NotFound)?;
    let canonical_candidate = candidate.canonicalize().map_err(|_| ServeError::NotFound)?;
    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(ServeError::Forbidden);
    }
    Ok(canonical_candidate)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib test_safe_join`
Expected: PASS, 2 passed.

- [ ] **Step 6: Commit**

```bash
git add src/process/http_serve.rs
git commit -m "feat(http-serve): add safe_join helper with traversal protection"
```

---

## Task 6: `render_breadcrumb` and `render_listing`

**Files:**
- Modify: `src/process/http_serve.rs`

- [ ] **Step 1: Write the failing test for `render_breadcrumb`**

Append to `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn test_render_breadcrumb_root() {
        let root = PathBuf::from(".").canonicalize().unwrap();
        let html = render_breadcrumb(&root, &root);
        assert!(html.contains(&format!(">{}<", root.file_name().unwrap().to_string_lossy())));
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib test_render_breadcrumb`
Expected: FAIL with compile error `cannot find function render_breadcrumb`.

- [ ] **Step 3: Implement `render_breadcrumb`**

Add to `src/process/http_serve.rs`:

```rust
fn render_breadcrumb(root: &Path, current: &Path) -> String {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        "<a href=\"/\">{}</a>",
        html_escape(&canonical_root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "/".to_string()))
    ));
    if let Ok(rel) = current.strip_prefix(&canonical_root) {
        let mut accum = String::new();
        for comp in rel.components() {
            let seg = comp.as_os_str().to_string_lossy();
            accum.push('/');
            accum.push_str(&seg);
            parts.push(format!("<a href=\"{}\">{}</a>", html_escape(&accum), html_escape(&seg)));
        }
    }
    parts.join(" / ")
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib test_render_breadcrumb`
Expected: PASS, 2 passed.

- [ ] **Step 5: Write the failing test for `render_listing`**

Append to `#[cfg(test)] mod tests`:

```rust
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
```

- [ ] **Step 6: Run test to verify it fails**

Run: `cargo test --lib test_render_listing`
Expected: FAIL with compile error `cannot find function render_listing`.

- [ ] **Step 7: Implement `render_listing`**

Add the imports needed (add to the `use` block):

```rust
use tokio_util::io::ReaderStream;
```

Add the `ListingEntry` struct and the `render_listing` function:

```rust
#[derive(Debug)]
struct ListingEntry {
    name: String,
    is_dir: bool,
    size: Option<u64>,
    mtime: Option<std::time::SystemTime>,
}

async fn collect_entries(dir: &Path) -> Result<Vec<ListingEntry>, ServeError> {
    let mut out = Vec::new();
    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let meta = match tokio::fs::symlink_metadata(entry.path()).await {
            Ok(m) => m,
            Err(_) => ListingEntry { name, is_dir: false, size: None, mtime: None },
        };
        let is_dir = meta.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let size = if is_dir { None } else { Some(meta.len()) };
        let mtime = meta.modified().ok();
        out.push(ListingEntry { name, is_dir, size, mtime });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn render_entry_row(parent_href: &str, e: &ListingEntry) -> String {
    let href = if e.is_dir {
        format!("{}/{}/", parent_href.trim_end_matches('/'), html_escape(&e.name))
    } else {
        format!("{}/{}", parent_href.trim_end_matches('/'), html_escape(&e.name))
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
            dt.map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()).unwrap_or_else(|| "—".to_string())
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

pub(crate) async fn render_listing(root: &Path, current: &Path) -> Result<String, ServeError> {
    let entries = collect_entries(current).await?;
    let breadcrumb = render_breadcrumb(root, current);
    let parent_link = if current != root.canonicalize().unwrap_or_else(|_| root.to_path_buf()) {
        let parent = current.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| root.to_path_buf());
        let rel = parent
            .strip_prefix(root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
            .ok()
            .and_then(|p| p.to_str())
            .unwrap_or("");
        let href = if rel.is_empty() { "/".to_string() } else { format!("/{}", rel) };
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
{parent}\n\
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
```

- [ ] **Step 8: Add `chrono` dependency for time formatting**

Run: `cargo add chrono --features clock`

Expected: `Cargo.toml` updates with:

```toml
chrono = { version = "0.4", features = ["clock"] }
```

- [ ] **Step 9: Run test to verify it passes**

Run: `cargo test --lib test_render_listing`
Expected: PASS, 1 passed.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml Cargo.lock src/process/http_serve.rs
git commit -m "feat(http-serve): add render_listing and render_breadcrumb"
```

---

## Task 7: `file_response` (streaming + MIME)

**Files:**
- Modify: `src/process/http_serve.rs`

- [ ] **Step 1: Write the failing test**

Append to `#[cfg(test)] mod tests`:

```rust
    #[tokio::test]
    async fn test_file_response_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hello.txt");
        std::fs::write(&p, b"hello world").unwrap();
        let resp = file_response(&p).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert_eq!(
            resp.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
            "text/plain"
        );
        assert_eq!(
            resp.headers().get(axum::http::header::CONTENT_LENGTH).unwrap(),
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
            resp.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
            "application/octet-stream"
        );
        assert_eq!(
            resp.headers().get(axum::http::header::CONTENT_LENGTH).unwrap(),
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib test_file_response`
Expected: FAIL with compile error `cannot find function file_response`.

- [ ] **Step 3: Implement `file_response`**

Add to `src/process/http_serve.rs`:

```rust
async fn file_response(path: &Path) -> Result<axum::response::Response, ServeError> {
    let meta = tokio::fs::metadata(path).await?;
    let len = meta.len();
    let file = tokio::fs::File::open(path).await?;
    let stream = ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);
    let resp = axum::response::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, mime_for(path).to_string())
        .header(axum::http::header::CONTENT_LENGTH, len)
        .body(body)
        .map_err(|e| ServeError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
    Ok(resp)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib test_file_response`
Expected: PASS, 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/process/http_serve.rs
git commit -m "feat(http-serve): add streaming file_response with mime and length"
```

---

## Task 8: `serve_handler` dispatcher, `dir_handler`, and router wiring

**Files:**
- Modify: `src/process/http_serve.rs` (rewrite the handler section; remove `/tower` route)

- [ ] **Step 1: Add `IntoResponse` impl for `ServeError`**

Add to `src/process/http_serve.rs`:

```rust
impl axum::response::IntoResponse for ServeError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        let (status, msg) = match self {
            ServeError::NotFound => (StatusCode::NOT_FOUND, "Not Found"),
            ServeError::Forbidden => (StatusCode::FORBIDDEN, "Forbidden"),
            ServeError::BadRequest => (StatusCode::BAD_REQUEST, "Bad Request"),
            ServeError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
        };
        (status, msg).into_response()
    }
}
```

- [ ] **Step 2: Implement `serve_handler` and `dir_handler`**

Add to `src/process/http_serve.rs`:

```rust
async fn serve_handler(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<HttpServeState>>,
    axum::extract::Path(path): axum::extract::Path<String>,
) -> Result<axum::response::Response, ServeError> {
    let abs = safe_join(&state.path, &path)?;
    let meta = tokio::fs::symlink_metadata(&abs).await.map_err(|_| ServeError::NotFound)?;
    if meta.is_dir() {
        dir_handler(state, abs).await
    } else {
        tracing::info!("Streaming {:?} ({} bytes)", abs, meta.len());
        Ok(file_response(&abs).await?)
    }
}

async fn dir_handler(
    state: std::sync::Arc<HttpServeState>,
    abs: PathBuf,
) -> Result<axum::response::Response, ServeError> {
    let index = abs.join("index.html");
    if tokio::fs::try_exists(&index).await.unwrap_or(false) {
        if let Ok(meta) = tokio::fs::metadata(&index).await {
            if meta.is_file() {
                tracing::info!("Serving {:?} (index.html, {} bytes)", abs, meta.len());
                return Ok(file_response(&index).await?);
            }
        }
    }
    let html = render_listing(&state.path, &abs).await?;
    let resp = axum::response::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(axum::body::Body::from(html))
        .map_err(|e| ServeError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
    Ok(resp)
}
```

- [ ] **Step 3: Rewrite `process_http_serve` to use the new dispatcher and accept a listener**

Replace the body of `process_http_serve` (in `src/process/http_serve.rs`) with:

```rust
pub async fn process_http_serve(path: PathBuf, port: u16) -> Result<()> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Serving {:?} on {}", path, listener.local_addr()?);
    serve(listener, path).await
}

pub async fn serve(listener: tokio::net::TcpListener, path: PathBuf) -> Result<()> {
    let state = HttpServeState { path };
    let router = axum::Router::new()
        .route("/*path", axum::routing::get(serve_handler))
        .with_state(std::sync::Arc::new(state));
    axum::serve(listener, router).await?;
    Ok(())
}
```

- [ ] **Step 4: Replace the old unit test**

Delete the existing `test_file_handler` test (it tests the obsolete `(StatusCode, String)` return shape) and replace it with a dispatcher smoke test:

```rust
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
        let body = axum::body::to_bytes(resp.into_body(), 16 * 1024).await.unwrap();
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
        let body = axum::body::to_bytes(resp.into_body(), 16 * 1024).await.unwrap();
        assert_eq!(&body[..], b"<h1>INDEX</h1>");
    }
```

- [ ] **Step 5: Run the full unit test suite**

Run: `cargo test --lib`
Expected: all unit tests pass; in particular `test_serve_handler_root_lists_dir`, `test_serve_handler_traversal_forbidden`, `test_serve_handler_index_html_precedence`, `test_file_response_text|binary|missing`, `test_render_listing`, `test_render_breadcrumb_*`, `test_html_escape`, `test_human_size`, `test_mime_for`, `test_safe_join_*`. The old `test_file_handler` is gone.

- [ ] **Step 6: Run clippy and fix warnings**

Run: `cargo clippy --tests -- -D warnings`
Expected: no warnings. If `clippy` complains about `chrono::DateTime::from_timestamp` returning `Option`, the code already handles it.

- [ ] **Step 7: Commit**

```bash
git add src/process/http_serve.rs
git commit -m "feat(http-serve): wire dispatcher, dir handler, and remove /tower route"
```

---

## Task 9: Integration tests with `reqwest`

**Files:**
- Create: `tests/http_serve.rs`

- [ ] **Step 1: Write the failing integration tests**

Create `tests/http_serve.rs`:

```rust
use std::sync::Arc;

use rcli::process::http_serve::serve;
use rcli::HttpServeState;

#[tokio::test]
async fn integration_root_renders_listing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("a.txt"), b"alpha").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let root_for_server = root.clone();
    let handle = tokio::spawn(async move {
        let _ = serve(listener, root_for_server).await;
    });

    let url = format!("http://{}/", addr);
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let ct = resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap().to_str().unwrap().to_string();
    assert!(ct.starts_with("text/html"), "content-type was {}", ct);
    let body = resp.text().await.unwrap();
    assert!(body.contains("a.txt"));
    assert!(body.contains("sub/"));

    handle.abort();
}

#[tokio::test]
async fn integration_text_file_served() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("hello.txt"), b"hello world").unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let root_for_server = root.clone();
    let handle = tokio::spawn(async move {
        let _ = serve(listener, root_for_server).await;
    });

    let url = format!("http://{}/hello.txt", addr);
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        "text/plain"
    );
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello world");

    handle.abort();
}

#[tokio::test]
async fn integration_missing_returns_404() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let root_for_server = root.clone();
    let handle = tokio::spawn(async move {
        let _ = serve(listener, root_for_server).await;
    });

    let url = format!("http://{}/missing.txt", addr);
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    handle.abort();
}

#[tokio::test]
async fn integration_binary_file_download() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let payload: Vec<u8> = (0u8..=255).cycle().take(2048).collect();
    std::fs::write(root.join("blob.bin"), &payload).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let root_for_server = root.clone();
    let handle = tokio::spawn(async move {
        let _ = serve(listener, root_for_server).await;
    });

    let url = format!("http://{}/blob.bin", addr);
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        "application/octet-stream"
    );
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len(), payload.len());
    assert_eq!(&body[..], &payload[..]);

    handle.abort();
}

#[tokio::test]
async fn integration_index_html_takes_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("index.html"), b"<title>INDEX</title>").unwrap();
    std::fs::write(root.join("other.txt"), b"x").unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let root_for_server = root.clone();
    let handle = tokio::spawn(async move {
        let _ = serve(listener, root_for_server).await;
    });

    let url = format!("http://{}/", addr);
    let resp = reqwest::get(&url).await.unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(body, "<title>INDEX</title>");

    handle.abort();
}

#[tokio::test]
async fn integration_traversal_returns_403() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let root_for_server = root.clone();
    let handle = tokio::spawn(async move {
        let _ = serve(listener, root_for_server).await;
    });

    let url = format!("http://{}/../../etc/passwd", addr);
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);

    handle.abort();
}
```

- [ ] **Step 2: Make `serve` reachable from integration tests**

The integration test imports `rcli::process::http_serve::serve`. To make this path visible, add a re-export. Edit `src/process/mod.rs`:

```rust
mod b64;
mod csv_convert;
mod gen_pass;
pub mod http_serve;
mod text;

pub use b64::{process_decode, process_encode};
pub use csv_convert::process_csv;
pub use gen_pass::process_genpass;
pub use http_serve::process_http_serve;
pub use text::{process_text_key_generate, process_text_sign, process_text_verify};
```

Note: only `http_serve` is `pub mod` (not just `mod`); the rest stay private to keep the public surface area unchanged.

- [ ] **Step 3: Remove unused `HttpServeState` import from the test (it was illustrative)**

The integration test imports `HttpServeState` but does not use it (because the listener is set up by the test directly, not through a state struct). Remove that line:

```rust
use rcli::process::http_serve::serve;
```

(Keep only the `serve` import.)

- [ ] **Step 4: Run integration tests**

Run: `cargo test --test http_serve`
Expected: all 6 integration tests pass.

If any test fails with "address already in use", confirm the listener was bound with port `0`; with port 0 the OS picks a free ephemeral port so collisions should not happen. If a test flakes because `handle.abort()` does not stop the server immediately, add `tokio::time::sleep(std::time::Duration::from_millis(50)).await;` before assertions or after `abort()`. Document any added waits in the test code.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test`
Expected: every unit and integration test passes.

- [ ] **Step 6: Commit**

```bash
git add src/process/mod.rs tests/http_serve.rs
git commit -m "test(http-serve): add integration tests for listing, streaming, and traversal"
```

---

## Task 10: Final verification and CHANGELOG

**Files:**
- Modify: `CHANGELOG.md` (optional but consistent with project convention)
- (No source changes expected.)

- [ ] **Step 1: Verify formatting and lints**

Run:
```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: all three succeed. If `cargo fmt` rewrites anything, commit the formatting change separately:

```bash
git add -u
git commit -m "style: cargo fmt"
```

- [ ] **Step 2: Smoke-test the CLI**

Run:
```bash
cargo run -- http serve -p 18080 -d .
```

In another terminal:
```bash
curl -i http://127.0.0.1:18080/
curl -i http://127.0.0.1:18080/Cargo.toml
curl -i http://127.0.0.1:18080/Cargo.lock
curl -i http://127.0.0.1:18080/missing
```

Expected:
- `GET /` → 200, `text/html`, body lists files including `Cargo.toml`, `Cargo.lock`, `src/`, etc.
- `GET /Cargo.toml` → 200, `text/plain`, body starts with `[package]`.
- `GET /Cargo.lock` → 200, `application/octet-stream` (or similar non-text), full body length > 0.
- `GET /missing` → 404.

Stop the server with `Ctrl-C`.

- [ ] **Step 3: Update CHANGELOG**

Run: `git cliff --tag unreleased --strip header 2>/dev/null || true`

If `git-cliff` is installed, this will refresh the changelog automatically. If it isn't, edit `CHANGELOG.md` to add an entry under `[unreleased]` / `### Features`:

```markdown
- add directory browsing and binary download support to http serve - ([<commit>](<url>))
```

Where `<commit>` is the SHA of Task 8's commit and `<url>` is `https://github.com/<owner>/<repo>/commit/<sha>`.

- [ ] **Step 4: Commit CHANGELOG if changed**

```bash
git add CHANGELOG.md
git commit -m "chore: update changelog for http serve browsing"
```

(If `CHANGELOG.md` had no changes, skip this commit.)

---

## Self-Review Notes (filled by plan author)

- **Spec coverage:** every behaviour, component, and test listed in the spec is implemented in a task. `safe_join` (Task 5), `mime_for` (Task 4), `html_escape` (Task 2), `human_size` (Task 3), `render_listing`/`render_breadcrumb` (Task 6), `file_response` (Task 7), dispatcher + `dir_handler` (Task 8), integration tests (Task 9), CHANGELOG (Task 10). Index.html precedence is covered by both unit and integration tests. Traversal protection is covered by unit and integration tests. Binary streaming is covered by both.
- **Placeholders:** none. Every code block is complete.
- **Type consistency:** `HttpServeState`, `ServeError`, `serve_handler`, `dir_handler`, `file_response`, `safe_join`, `render_listing`, `render_breadcrumb`, `mime_for`, `html_escape`, `human_size` are all defined where they are first referenced, and their signatures are reused identically in later tasks. `serve` is exposed `pub` in Task 8 and re-imported in Task 9 under `rcli::process::http_serve::serve`.
- **One known issue caught during self-review:** the integration test in Task 9 originally imported `HttpServeState` but does not use it. Step 3 of Task 9 removes that import. The unit tests in Task 8 still construct `HttpServeState` directly and need the type to be `pub`-visible inside the crate, which it already is.
- **Bug caught and fixed during self-review:** the original Task 8 `dir_handler` called `render_listing(&abs.parent().unwrap_or(&abs), &abs)`, but `render_listing(root, current)` expects `root` to be the served root. Passing `abs.parent()` would have produced breadcrumbs starting from `/` instead of the user's serve directory. Fixed to pass `&state.path` and renamed the `_state` parameter to `state` so it is actually used.

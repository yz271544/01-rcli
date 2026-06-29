# http serve — directory browsing & binary download

**Date:** 2026-06-29
**Status:** Approved (pending user spec review)
**Owner:** rcli

## Problem

`rcli http serve` (`src/process/http_serve.rs`) currently:

1. Has an explicit TODO to render a directory listing when a path resolves to a directory.
2. Reads files with `tokio::fs::read_to_string`, which fails on any non-UTF-8 binary file. Binary downloads are effectively broken on the public route (the hidden `/tower` `ServeDir` route works but is dead code).

The user has asked to:

- Add directory file browsing (matching the existing TODO).
- Add non-text file download support so that binaries can be served correctly.

## Scope

In scope:

- Replace the manual `read_to_string` path with streaming binary support.
- Render an HTML directory listing (with metadata) when a directory is requested, preferring `index.html` when present.
- Add `ServeError` and a security check against directory traversal.
- Unit tests and integration tests for both behaviours.

Out of scope:

- New CLI flags (sort / hidden-file toggles). YAGNI: defaults cover the common case.
- A template engine. Inline HTML string assembly is sufficient.
- Authentication, TLS, access logs beyond `tracing`.

## Behaviour

### Directory handling

When the resolved path is a directory:

1. If `<dir>/index.html` exists and is a regular file, serve it using the same streaming code path as any other file.
2. Otherwise, render an HTML listing page.

### Listing page

- Sort: file name, ascending, byte-order.
- Hidden files: entries whose name starts with `.` are excluded.
- Symlinks: `symlink_metadata` is used, so the listed size/mtime reflect the link itself, not the target. A broken link shows `—` for size/mtime and is still listed.
- Columns: `Name`, `Size` (human-readable, IEC units), `Modified` (ISO 8601 in UTC).
- Subdirectory entries link with a trailing `/`. Files link without one.
- A `..` entry is rendered when `current != root`.
- A breadcrumb of segments from `root` to `current` is shown at the top.
- File names and breadcrumb segments are HTML-escaped.

### File handling

- Files are streamed using `tokio::fs::File` wrapped in `tokio_util::io::ReaderStream`, returned as `axum::body::Body::from_stream`.
- `Content-Type` is derived from the extension via `mime_guess`; missing extension falls back to `application/octet-stream`.
- `Content-Length` is set from `metadata.len()` before opening the file, so the header is known up front.

### Security

- All paths are resolved via `safe_join(root, rel)`, which `canonicalize`s the candidate and rejects any result that does not start with `canonicalize(root)`.
- `..` segments, encoded traversal attempts (`%2E%2E%2F`), and absolute paths injected via the URL are all rejected.
- Error responses never include the absolute filesystem path.

### Errors

| Variant           | Status | Body                  |
| ----------------- | ------ | --------------------- |
| `NotFound`        | 404    | `Not Found`           |
| `Forbidden`       | 403    | `Forbidden`           |
| `BadRequest`      | 400    | `Bad Request`         |
| `Io(io::Error)`   | 500    | `Internal Server Error` |

Log levels:

- `info!` for 2xx responses.
- `warn!` for 4xx responses.
- `error!` for 5xx responses (includes the internal path for debugging).

## Architecture

Only `Cargo.toml` and `src/process/http_serve.rs` change. `src/cli/` is untouched.

### `Cargo.toml`

- Add direct dep: `tokio-util = { version = "0.7", features = ["io"] }`.
- Promote transitive dep to direct: `mime_guess = "2"`.
- Dev dep for integration tests: `reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }`.

### `src/process/http_serve.rs`

- Delete `nest_service("/tower", ServeDir::new(path))` (dead code in the new design).
- Replace `file_handler` with `serve_handler`, which dispatches by metadata type.
- Add internal units:

  - `safe_join(root: &Path, rel: &str) -> Result<PathBuf, ServeError>`
  - `file_response(abs: &Path) -> Result<Response, ServeError>`
  - `dir_handler(State, Path) -> Result<Response, ServeError>`
  - `render_listing(root: &Path, current: &Path) -> Result<String, ServeError>`
  - `render_breadcrumb(root: &Path, current: &Path) -> String`
  - `mime_for(abs: &Path) -> mime::Mime`
  - `html_escape(s: &str) -> Cow<'_, str>`
  - `human_size(bytes: u64) -> String`

- Introduce `ServeError` enum with `IntoResponse`.

### Data flow

```
GET /some/path
  → serve_handler
    → safe_join(root, "some/path")
      ! Err → ServeError response
    → tokio::fs::symlink_metadata(abs)
      ! Err → 404
    → is_dir() ?
        yes → dir_handler
                → abs/index.html exists & is file ?
                    yes → file_response(abs/index.html)
                    no  → render_listing(root, abs) → 200 text/html
        no  → file_response(abs)
                → ReaderStream + mime_guess → 200 + body
```

### State

`HttpServeState { path: PathBuf }` is unchanged in shape; it remains wrapped in `Arc` and shared via `axum::extract::State`.

## Components

### `safe_join(root, rel) -> Result<PathBuf, ServeError>`

- `Path::join(root, rel)` then `canonicalize()`.
- Compare `candidate.starts_with(root.canonicalize())`; otherwise `Forbidden`.
- `canonicalize` failure → `NotFound`.
- URL decoding is handled by `axum::extract::Path` before this function is called; the function itself only works on already-decoded `&str`.

### `file_response(abs) -> Result<Response>`

- `tokio::fs::metadata(abs)` → `Content-Length`.
- `tokio::fs::File::open(abs)` → `ReaderStream::new(file)`.
- `Content-Type`: `mime_guess::from_path(abs).first_or_octet_stream()`.
- Body: `axum::body::Body::from_stream(stream)`.
- All I/O errors → `ServeError::Io`.

### `render_listing(root, current) -> Result<String>`

- `tokio::fs::read_dir(current)` (non-recursive).
- Filter out names starting with `.`.
- Sort by name (byte order).
- For each entry: `symlink_metadata`; if `Err`, show `—` for size/mtime but still list.
- Render `<table>` with three columns.
- Render breadcrumb at the top, parent link at the top of the body (when `current != root`).

### `dir_handler(State, Path) -> Result<Response>`

- Resolve via `safe_join`.
- Probe `<abs>/index.html`; if it exists and is a regular file, delegate to `file_response`.
- Otherwise, render the listing.

### `mime_for(abs) -> mime::Mime`

Thin wrapper around `mime_guess::from_path(abs).first_or_octet_stream()`. Centralises the fallback so the rule lives in one place.

### `html_escape(s) -> Cow<'_, str>`

- Replaces `&`, `<`, `>`, `"`, `'` with their entity references.
- No external dependency.

### `human_size(bytes) -> String`

- IEC units: `B`, `KiB`, `MiB`, `GiB`, `TiB`, `PiB`.
- Two significant figures after the decimal when `bytes >= 1024`; otherwise integer bytes.

## Testing

### Unit tests (`src/process/http_serve.rs`)

1. `test_safe_join_basic`: file inside root resolves; `""` and `"."` resolve to root.
2. `test_safe_join_traversal`: `"../outside"` and `"subdir/../../etc/passwd"` → `Forbidden`; non-existent path → `NotFound`.
3. `test_render_listing`: build a `tempfile::tempdir` with mixed entries (file, subdir, hidden file, `index.html`), assert the rendered HTML:
   - contains the visible file names
   - does not contain the hidden file name
   - subdirectory links end in `/`
   - file links do not end in `/`
   - contains the `index.html` link
   - size column is present
   - when `current` is a subdir, `..` link is present
4. `test_html_escape`: input containing `<script>`, `&`, `"` is escaped correctly.
5. `test_mime_for`: `.html` → `text/html`; `.png` → `image/png`; `.unknown` → `application/octet-stream`.

### Integration tests (`tests/http_serve.rs`, new file)

Requires `process_http_serve` to bind to port `0` (OS-assigned). Refactor: `process_http_serve` accepts the port as today; in tests, spawn it on `0` and read the bound port via `tokio::net::TcpListener::local_addr()`.

Use `reqwest` to:

1. `GET /` on a tempdir returns `text/html` and contains the `index.html` link.
2. `GET /Cargo.toml` (or any text file in fixtures) returns 200 with `text/plain`-ish content and includes `[package]` (when run from project root).
3. `GET /missing` returns 404.
4. `GET /fixtures/<binary>` (using a known fixture) returns 200 with the correct binary `Content-Type` and matching byte length.
5. `GET /` on a directory containing `index.html` returns the file's bytes (not the listing).
6. `GET /..%2F..%2Fetc%2Fpasswd` returns 403.

### Not tested

- Real large-file streaming (a 100 KB mock payload is sufficient).
- Cross-platform path edge cases; the project already assumes Unix paths.

## Migration / cleanup

- The `/tower` route is removed.
- The old `file_handler` is replaced; the existing `test_file_handler` test must be updated to match the new return type (`Response` instead of `(StatusCode, String)`), or split into new tests.

## Open questions

None.

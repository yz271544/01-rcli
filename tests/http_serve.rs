use rcli::serve;

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
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
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
async fn integration_traversal_normalized_404() {
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
    // Hyper's HTTP parser normalizes ".." in the path before routing, so
    // "../../etc/passwd" is resolved to "etc/passwd". Since that file does
    // not exist inside the temp dir, the server returns 404 Not Found.
    // The actual path-traversal check is handled by safe_join canonicalization
    // (catches symlink escapes), not by raw URI string matching.
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    handle.abort();
}

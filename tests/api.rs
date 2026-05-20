/// Integration tests for the gif server API.
///
/// Requires a running Postgres instance. DATABASE_URL is read from .env.
/// Each test gets its own isolated database via sqlx::test.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use std::sync::Arc;
use tower::ServiceExt;

use gif_server::{auth::mint_session, config::Config, AppState, create_router};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const TEST_SECRET: &str = "test-session-secret";
const TEST_SERVER: &str = "test.server";
const TEST_USER: &str = "@user:test.server";
const TEST_OTHER: &str = "@other:test.server";
const TEST_ADMIN: &str = "@admin:test.server";

fn test_config(storage_path: String, max_bytes: u64) -> Config {
    Config {
        database_url: String::new(),
        storage_path,
        host: "127.0.0.1".to_string(),
        port: 8847,
        base_url: "http://localhost:8847".to_string(),
        matrix_server_name: TEST_SERVER.to_string(),
        matrix_federation_url: "http://localhost:0".to_string(),
        admin_mxids: vec![TEST_ADMIN.to_string()],
        session_secret: TEST_SECRET.to_string(),
        storage_max_bytes: max_bytes,
        per_user_storage_bytes: max_bytes,
        cors_allowed_origins: vec![],
    }
}

fn make_state_with_cap(pool: PgPool, max_bytes: u64) -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config(dir.path().to_str().unwrap().to_string(), max_bytes);
    let state = AppState {
        pool,
        config: Arc::new(config),
        http: reqwest::Client::new(),
    };
    (state, dir)
}

fn make_state(pool: PgPool) -> (AppState, tempfile::TempDir) {
    make_state_with_cap(pool, u64::MAX)
}

/// Build an `Authorization: Bearer` header value for the given mxid, signed
/// with the same secret the test server uses.
fn bearer(mxid: &str) -> String {
    let config = test_config(String::new(), u64::MAX);
    let (token, _) = mint_session(&config, mxid).unwrap();
    format!("Bearer {token}")
}

/// Build a minimal valid animated GIF.
fn make_gif(width: u16, height: u16, frames: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let pixels = vec![100u8; width as usize * height as usize * 4];
    let mut enc = gif::Encoder::new(&mut out, width, height, &[]).unwrap();
    enc.set_repeat(gif::Repeat::Infinite).unwrap();
    for _ in 0..frames {
        let mut frame = gif::Frame::from_rgba_speed(width, height, &mut pixels.clone(), 1);
        frame.delay = 10;
        enc.write_frame(&frame).unwrap();
    }
    drop(enc);
    out
}

/// Build a multipart/form-data body with a GIF file and optional tags.
fn multipart_body(gif_data: &[u8], tags: Option<&str>, boundary: &str) -> Vec<u8> {
    let mut body = Vec::new();
    // file part
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"test.gif\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: image/gif\r\n\r\n");
    body.extend_from_slice(gif_data);
    body.extend_from_slice(b"\r\n");
    // tags part
    if let Some(t) = tags {
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"tags\"\r\n\r\n");
        body.extend_from_slice(t.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
    body
}

async fn body_json(body: axum::body::Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

// ---------------------------------------------------------------------------
// Auth tests
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn missing_session_returns_401(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);

    let res = app
        .oneshot(Request::get("/gifs/search").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(res.into_body()).await;
    assert_eq!(json["error"], "unauthorized");
}

#[sqlx::test]
async fn invalid_session_returns_401(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::get("/gifs/search")
                .header("Authorization", "Bearer not-a-real-jwt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Search / list tests
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn search_returns_empty_initially(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::get("/gifs/search")
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let json = body_json(res.into_body()).await;
    assert_eq!(json["results"].as_array().unwrap().len(), 0);
    assert!(json["next"].is_null());
}

// ---------------------------------------------------------------------------
// Upload tests
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn upload_gif_returns_metadata(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let gif = make_gif(100, 80, 3);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, Some("nature,birds"), boundary);

    let res = app
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", &key)
                .header(
                    "Content-Type",
                    format!("multipart/form-data; boundary={}", boundary),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
    let json = body_json(res.into_body()).await;
    assert!(!json["id"].as_str().unwrap().is_empty());
    assert_eq!(json["frame_count"], 3);
    assert_eq!(json["duration_ms"], 300);
    let tags = json["tags"].as_array().unwrap();
    assert!(tags.contains(&Value::String("nature".into())));
    assert!(tags.contains(&Value::String("birds".into())));
    assert!(json["renditions"]["original"]["url"].is_string());
    assert!(json["renditions"]["preview"]["url"].is_string());
    assert!(json["renditions"]["thumbnail"]["url"].is_string());
}

#[sqlx::test]
async fn upload_duplicate_returns_existing(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, None, boundary);

    let res1 = app
        .clone()
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", &key)
                .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res1.status(), StatusCode::CREATED);
    let id1 = body_json(res1.into_body()).await["id"].as_str().unwrap().to_string();

    let res2 = app
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", &key)
                .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::OK); // 200, not 201
    let id2 = body_json(res2.into_body()).await["id"].as_str().unwrap().to_string();

    assert_eq!(id1, id2); // same GIF returned
}

#[sqlx::test]
async fn upload_invalid_file_returns_400(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let boundary = "testboundary123";
    let body = multipart_body(b"this is not a gif", None, boundary);

    let res = app
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", &key)
                .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn upload_too_many_tags_gets_capped(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    // Send 30 tags, expect only 20 stored
    let many_tags = (0..30).map(|i| format!("tag{}", i)).collect::<Vec<_>>().join(",");
    let body = multipart_body(&gif, Some(&many_tags), boundary);

    let res = app
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", &key)
                .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
    let json = body_json(res.into_body()).await;
    assert_eq!(json["tags"].as_array().unwrap().len(), 20);
}

// ---------------------------------------------------------------------------
// Search by tag
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn search_finds_gif_by_tag(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, Some("cats,funny"), boundary);
    app.clone()
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", &key)
                .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    let res = app
        .oneshot(
            Request::get("/gifs/search?q=cats")
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let json = body_json(res.into_body()).await;
    assert_eq!(json["results"].as_array().unwrap().len(), 1);
}

#[sqlx::test]
async fn search_returns_no_results_for_unknown_tag(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::get("/gifs/search?q=doesnotexist")
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res.into_body()).await["results"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// Select / uses
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn select_increments_uses(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    // Upload
    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, None, boundary);
    let upload_res = app
        .clone()
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", &key)
                .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let id = body_json(upload_res.into_body()).await["id"].as_str().unwrap().to_string();

    // Select
    let sel_res = app
        .clone()
        .oneshot(
            Request::post(format!("/gifs/{}/select", id))
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(sel_res.status(), StatusCode::NO_CONTENT);

    // Fetch and check uses
    let get_res = app
        .oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(get_res.into_body()).await;
    assert_eq!(json["uses"], 1);
}

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn put_tags_replaces_existing(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, Some("old-tag"), boundary);
    let upload_json = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let id = upload_json["id"].as_str().unwrap();

    app.clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/gifs/{}/tags", id))
                .header("Authorization", &key)
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"tags":["new-tag"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    let get_res = app
        .oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(get_res.into_body()).await;
    let tags = json["tags"].as_array().unwrap();
    assert!(!tags.contains(&Value::String("old-tag".into())));
    assert!(tags.contains(&Value::String("new-tag".into())));
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn delete_gif_removes_it(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, None, boundary);
    let upload_json = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let id = upload_json["id"].as_str().unwrap().to_string();

    let del_res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/gifs/{}", id))
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del_res.status(), StatusCode::NO_CONTENT);

    let get_res = app
        .oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_res.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Tag suggestions
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn suggest_returns_matching_tags(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, Some("cartoon,castle,cat"), boundary);
    app.clone()
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", &key)
                .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    let res = app
        .oneshot(
            Request::get("/gifs/tags/suggest?q=ca")
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let json = body_json(res.into_body()).await;
    let suggestions: Vec<String> = json["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(suggestions.iter().all(|s| s.starts_with("ca")));
    assert!(!suggestions.is_empty());
}

// ---------------------------------------------------------------------------
// Fetch single GIF
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn get_gif_returns_metadata(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, Some("test"), boundary);
    let upload_json = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let id = upload_json["id"].as_str().unwrap().to_string();

    let res = app
        .oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let json = body_json(res.into_body()).await;
    assert_eq!(json["id"], id.as_str());
}

#[sqlx::test]
async fn get_nonexistent_gif_returns_404(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::get("/gifs/00000000-0000-0000-0000-000000000000")
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// File serving
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn serve_original_returns_gif_content_type(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, None, boundary);
    let id = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let res = app
        .oneshot(
            Request::get(format!("/gifs/{}/file", id))
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "image/gif"
    );
}

#[sqlx::test]
async fn serve_thumbnail_returns_png_content_type(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, None, boundary);
    let id = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let res = app
        .oneshot(
            Request::get(format!("/gifs/{}/file?rendition=thumbnail", id))
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "image/png"
    );
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn pagination_cursor_advances_pages(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    // Upload 3 GIFs with different pixel values so hashes differ
    let boundary = "testboundary123";
    for shade in [50u8, 100, 150] {
        let mut pixels = vec![shade; 50 * 50 * 4];
        let mut out = Vec::new();
        let mut enc = gif::Encoder::new(&mut out, 50, 50, &[]).unwrap();
        enc.set_repeat(gif::Repeat::Infinite).unwrap();
        let mut frame = gif::Frame::from_rgba_speed(50, 50, &mut pixels, 1);
        frame.delay = 10;
        enc.write_frame(&frame).unwrap();
        drop(enc);
        let body = multipart_body(&out, None, boundary);
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
    }

    // Page 1: limit=2
    let res1 = app
        .clone()
        .oneshot(
            Request::get("/gifs/recent?limit=2")
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json1 = body_json(res1.into_body()).await;
    assert_eq!(json1["results"].as_array().unwrap().len(), 2);
    let cursor = json1["next"].as_str().unwrap().to_string();

    // Page 2: use cursor
    let res2 = app
        .oneshot(
            Request::get(format!("/gifs/recent?limit=2&pos={}", cursor))
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json2 = body_json(res2.into_body()).await;
    assert_eq!(json2["results"].as_array().unwrap().len(), 1);
    assert!(json2["next"].is_null()); // no more pages

    // IDs across pages are distinct
    let ids1: Vec<&str> = json1["results"].as_array().unwrap()
        .iter().map(|g| g["id"].as_str().unwrap()).collect();
    let ids2: Vec<&str> = json2["results"].as_array().unwrap()
        .iter().map(|g| g["id"].as_str().unwrap()).collect();
    assert!(ids1.iter().all(|id| !ids2.contains(id)));
}

// ---------------------------------------------------------------------------
// Featured ordering
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn featured_returns_most_selected_first(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let boundary = "testboundary123";

    // Upload two distinct GIFs
    let gif_a = make_gif(40, 40, 1);
    let gif_b: Vec<u8> = {
        let mut pixels = vec![200u8; 40 * 40 * 4];
        let mut out = Vec::new();
        let mut enc = gif::Encoder::new(&mut out, 40, 40, &[]).unwrap();
        enc.set_repeat(gif::Repeat::Infinite).unwrap();
        let mut frame = gif::Frame::from_rgba_speed(40, 40, &mut pixels, 1);
        frame.delay = 10;
        enc.write_frame(&frame).unwrap();
        drop(enc);
        out
    };

    let _id_a = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(multipart_body(&gif_a, None, boundary)))
                    .unwrap(),
            )
            .await.unwrap().into_body(),
    ).await["id"].as_str().unwrap().to_string();

    let id_b = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(multipart_body(&gif_b, None, boundary)))
                    .unwrap(),
            )
            .await.unwrap().into_body(),
    ).await["id"].as_str().unwrap().to_string();

    // Select GIF B twice
    for _ in 0..2 {
        app.clone()
            .oneshot(
                Request::post(format!("/gifs/{}/select", id_b))
                    .header("Authorization", &key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    }

    let res = app
        .oneshot(
            Request::get("/gifs/featured")
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let json = body_json(res.into_body()).await;
    let first_id = json["results"][0]["id"].as_str().unwrap();
    assert_eq!(first_id, id_b);
}

// ---------------------------------------------------------------------------
// Recent ordering
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn recent_returns_newest_first(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let boundary = "testboundary123";
    let mut ids = Vec::new();
    for shade in [50u8, 100, 150] {
        let mut pixels = vec![shade; 30 * 30 * 4];
        let mut out = Vec::new();
        let mut enc = gif::Encoder::new(&mut out, 30, 30, &[]).unwrap();
        enc.set_repeat(gif::Repeat::Infinite).unwrap();
        let mut frame = gif::Frame::from_rgba_speed(30, 30, &mut pixels, 1);
        frame.delay = 10;
        enc.write_frame(&frame).unwrap();
        drop(enc);
        let body = multipart_body(&out, None, boundary);
        let id = body_json(
            app.clone()
                .oneshot(
                    Request::post("/gifs")
                        .header("Authorization", &key)
                        .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await.unwrap().into_body(),
        ).await["id"].as_str().unwrap().to_string();
        ids.push(id);
    }

    let res = app
        .oneshot(
            Request::get("/gifs/recent")
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let json = body_json(res.into_body()).await;
    let returned_ids: Vec<&str> = json["results"].as_array().unwrap()
        .iter().map(|g| g["id"].as_str().unwrap()).collect();

    // Last uploaded should be first
    assert_eq!(returned_ids[0], ids[2]);
    assert_eq!(returned_ids[1], ids[1]);
    assert_eq!(returned_ids[2], ids[0]);
}

// ---------------------------------------------------------------------------
// Tag operations
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn patch_tags_adds_without_removing(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let id = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(multipart_body(&gif, Some("original"), boundary)))
                    .unwrap(),
            )
            .await.unwrap().into_body(),
    ).await["id"].as_str().unwrap().to_string();

    app.clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/gifs/{}/tags", id))
                .header("Authorization", &key)
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"tags":["added"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    let json = body_json(
        app.oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await.unwrap().into_body(),
    ).await;

    let tags = json["tags"].as_array().unwrap();
    assert!(tags.contains(&Value::String("original".into())));
    assert!(tags.contains(&Value::String("added".into())));
}

#[sqlx::test]
async fn delete_tags_removes_specific_tags(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let id = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(multipart_body(&gif, Some("keep,remove"), boundary)))
                    .unwrap(),
            )
            .await.unwrap().into_body(),
    ).await["id"].as_str().unwrap().to_string();

    app.clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/gifs/{}/tags", id))
                .header("Authorization", &key)
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"tags":["remove"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    let json = body_json(
        app.oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await.unwrap().into_body(),
    ).await;

    let tags = json["tags"].as_array().unwrap();
    assert!(tags.contains(&Value::String("keep".into())));
    assert!(!tags.contains(&Value::String("remove".into())));
}

// ---------------------------------------------------------------------------
// 404 on missing resources
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn select_nonexistent_gif_returns_404(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::post("/gifs/00000000-0000-0000-0000-000000000000/select")
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn delete_nonexistent_gif_returns_404(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/gifs/00000000-0000-0000-0000-000000000000")
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Files removed from disk after delete
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn delete_removes_files_from_disk(pool: PgPool) {
    let (state, dir) = make_state(pool.clone());
    let storage_path = dir.path().to_str().unwrap().to_string();
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let upload_json = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(multipart_body(&gif, None, boundary)))
                    .unwrap(),
            )
            .await.unwrap().into_body(),
    ).await;
    let id = upload_json["id"].as_str().unwrap().to_string();

    // Collect rendition filenames from storage before delete
    let files_before: Vec<String> = walkdir::WalkDir::new(&storage_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_str().unwrap().to_string())
        .collect();
    assert!(!files_before.is_empty());

    app.clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/gifs/{}", id))
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let files_after: Vec<String> = walkdir::WalkDir::new(&storage_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_str().unwrap().to_string())
        .collect();
    assert!(files_after.is_empty());
}

// ---------------------------------------------------------------------------
// Tag suggestion wildcard safety
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn suggest_with_wildcard_chars_does_not_error(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::get("/gifs/tags/suggest?q=ca%25te%5F")
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    // Should return empty results, not an error
    let json = body_json(res.into_body()).await;
    assert!(json["suggestions"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Multi-term search ranking
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn search_ranks_more_matching_tags_first(pool: PgPool) {
    let (state, _dir) = make_state(pool.clone());
    let key = bearer(TEST_USER);
    let app = create_router(state);

    let boundary = "testboundary123";

    // GIF A: only "cat"
    let gif_a = make_gif(40, 40, 1);
    let _id_a = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(multipart_body(&gif_a, Some("cat"), boundary)))
                    .unwrap(),
            )
            .await.unwrap().into_body(),
    ).await["id"].as_str().unwrap().to_string();

    // GIF B: "cat" and "funny" — should rank higher for "cat funny"
    let gif_b: Vec<u8> = {
        let mut pixels = vec![200u8; 40 * 40 * 4];
        let mut out = Vec::new();
        let mut enc = gif::Encoder::new(&mut out, 40, 40, &[]).unwrap();
        enc.set_repeat(gif::Repeat::Infinite).unwrap();
        let mut frame = gif::Frame::from_rgba_speed(40, 40, &mut pixels, 1);
        frame.delay = 10;
        enc.write_frame(&frame).unwrap();
        drop(enc);
        out
    };
    let id_b = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(multipart_body(&gif_b, Some("cat,funny"), boundary)))
                    .unwrap(),
            )
            .await.unwrap().into_body(),
    ).await["id"].as_str().unwrap().to_string();

    let res = app
        .oneshot(
            Request::get("/gifs/search?q=cat+funny")
                .header("Authorization", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let json = body_json(res.into_body()).await;
    let first_id = json["results"][0]["id"].as_str().unwrap();
    assert_eq!(first_id, id_b);
}

// ---------------------------------------------------------------------------
// Matrix identity / visibility / quota
// ---------------------------------------------------------------------------

/// Multipart body with an explicit visibility field.
fn multipart_vis(gif_data: &[u8], visibility: &str, boundary: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"test.gif\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: image/gif\r\n\r\n");
    body.extend_from_slice(gif_data);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"visibility\"\r\n\r\n");
    body.extend_from_slice(visibility.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
    body
}

async fn upload_gif(app: &axum::Router, token: &str, gif: &[u8], boundary: &str) -> Value {
    body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", token)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(multipart_body(gif, None, boundary)))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await
}

#[sqlx::test]
async fn upload_records_uploader_and_default_visibility(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let json = upload_gif(&app, &bearer(TEST_USER), &make_gif(40, 40, 1), "b1").await;
    assert_eq!(json["uploader_id"], TEST_USER);
    assert_eq!(json["visibility"], "shared");
}

#[sqlx::test]
async fn private_gif_hidden_from_other_user_visible_to_owner(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);

    let res = app
        .clone()
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", bearer(TEST_USER))
                .header("Content-Type", "multipart/form-data; boundary=bv")
                .body(Body::from(multipart_vis(&make_gif(40, 40, 1), "private", "bv")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let id = body_json(res.into_body()).await["id"].as_str().unwrap().to_string();

    // Owner can fetch it.
    let owner_res = app
        .clone()
        .oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_USER))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(owner_res.status(), StatusCode::OK);

    // Another user gets 404 (existence not disclosed).
    let other_res = app
        .clone()
        .oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_OTHER))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(other_res.status(), StatusCode::NOT_FOUND);

    // And it's absent from another user's recent list.
    let list = body_json(
        app.oneshot(
            Request::get("/gifs/recent")
                .header("Authorization", bearer(TEST_OTHER))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body(),
    )
    .await;
    assert_eq!(list["results"].as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn owner_can_toggle_visibility(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let id = upload_gif(&app, &bearer(TEST_USER), &make_gif(40, 40, 1), "b2").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let patch = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_USER))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"visibility":"private"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patch.status(), StatusCode::NO_CONTENT);

    let other = app
        .oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_OTHER))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(other.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn non_owner_cannot_change_visibility(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let id = upload_gif(&app, &bearer(TEST_USER), &make_gif(40, 40, 1), "b3").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let res = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_OTHER))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"visibility":"private"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn non_owner_cannot_delete_but_admin_can(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let id = upload_gif(&app, &bearer(TEST_USER), &make_gif(40, 40, 1), "b4").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let forbidden = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_OTHER))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let admin = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_ADMIN))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(admin.status(), StatusCode::NO_CONTENT);
}

#[sqlx::test]
async fn upload_over_storage_cap_returns_507(pool: PgPool) {
    // 1 byte cap — the first upload's renditions exceed it.
    let (state, _dir) = make_state_with_cap(pool, 1);
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", bearer(TEST_USER))
                .header("Content-Type", "multipart/form-data; boundary=bc")
                .body(Body::from(multipart_body(&make_gif(60, 60, 2), None, "bc")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::INSUFFICIENT_STORAGE);
}

#[sqlx::test]
async fn auth_matrix_rejects_token_from_untrusted_homeserver(pool: PgPool) {
    // No session header (route is public). A token claiming a different
    // homeserver must be rejected before any network call.
    let (state, _dir) = make_state(pool);
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::post("/auth/matrix")
                .header("Content-Type", "application/json")
                .header("X-Forwarded-For", "1.2.3.4")
                .body(Body::from(
                    r#"{"access_token":"x","matrix_server_name":"evil.example"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(res.into_body()).await;
    assert_eq!(json["error"], "unauthorized");
}

#[sqlx::test]
async fn auth_matrix_attempts_federation_verification_for_trusted_homeserver(pool: PgPool) {
    // Correct homeserver and no session header: the handler runs (proving the
    // route bypasses the session middleware) and proceeds to contact the
    // federation endpoint, which is unreachable here -> 500, not 401/404.
    let (state, _dir) = make_state(pool);
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::post("/auth/matrix")
                .header("Content-Type", "application/json")
                .header("X-Forwarded-For", "1.2.3.4")
                .body(Body::from(
                    r#"{"access_token":"x","matrix_server_name":"test.server"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

// ---------------------------------------------------------------------------
// NSFW flag
// ---------------------------------------------------------------------------

/// Multipart body with an explicit nsfw field.
fn multipart_nsfw(gif_data: &[u8], nsfw: &str, boundary: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"test.gif\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: image/gif\r\n\r\n");
    body.extend_from_slice(gif_data);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"nsfw\"\r\n\r\n");
    body.extend_from_slice(nsfw.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
    body
}

#[sqlx::test]
async fn upload_defaults_is_nsfw_false(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let json = upload_gif(&app, &bearer(TEST_USER), &make_gif(40, 40, 1), "n1").await;
    assert_eq!(json["is_nsfw"], false);
}

#[sqlx::test]
async fn upload_with_nsfw_flag_is_recorded(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);

    let res = app
        .clone()
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", bearer(TEST_USER))
                .header("Content-Type", "multipart/form-data; boundary=bn")
                .body(Body::from(multipart_nsfw(&make_gif(40, 40, 1), "true", "bn")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let id = body_json(res.into_body()).await["id"].as_str().unwrap().to_string();

    let json = body_json(
        app.oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_USER))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body(),
    )
    .await;
    assert_eq!(json["is_nsfw"], true);
}

#[sqlx::test]
async fn owner_can_toggle_is_nsfw(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let id = upload_gif(&app, &bearer(TEST_USER), &make_gif(40, 40, 1), "n2").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let patch = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_USER))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"is_nsfw":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patch.status(), StatusCode::NO_CONTENT);

    let json = body_json(
        app.oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_USER))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body(),
    )
    .await;
    assert_eq!(json["is_nsfw"], true);
}

#[sqlx::test]
async fn patch_with_no_fields_returns_400(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let id = upload_gif(&app, &bearer(TEST_USER), &make_gif(40, 40, 1), "n3").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let res = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_USER))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn non_owner_cannot_set_nsfw(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let id = upload_gif(&app, &bearer(TEST_USER), &make_gif(40, 40, 1), "n4").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let res = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_OTHER))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"is_nsfw":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn admin_can_toggle_visibility_and_nsfw_on_shared_gif(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let id = upload_gif(&app, &bearer(TEST_USER), &make_gif(40, 40, 1), "a1").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let patch = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_ADMIN))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"visibility":"private","is_nsfw":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patch.status(), StatusCode::NO_CONTENT);

    // Owner still sees it; it's now private and flagged.
    let json = body_json(
        app.oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_USER))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body(),
    )
    .await;
    assert_eq!(json["visibility"], "private");
    assert_eq!(json["is_nsfw"], true);
}

// ---------------------------------------------------------------------------
// NSFW search filtering
// ---------------------------------------------------------------------------

async fn upload_tagged(
    app: &axum::Router,
    token: &str,
    gif: &[u8],
    tags: &str,
    nsfw: bool,
    boundary: &str,
) -> String {
    let body = if nsfw {
        // file + tags + nsfw=true
        let mut b = Vec::new();
        for (name, val) in [("tags", tags), ("nsfw", "true")] {
            b.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            b.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", name).as_bytes(),
            );
            b.extend_from_slice(val.as_bytes());
            b.extend_from_slice(b"\r\n");
        }
        let mut full = Vec::new();
        full.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        full.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"t.gif\"\r\n",
        );
        full.extend_from_slice(b"Content-Type: image/gif\r\n\r\n");
        full.extend_from_slice(gif);
        full.extend_from_slice(b"\r\n");
        full.extend_from_slice(&b);
        full.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
        full
    } else {
        multipart_body(gif, Some(tags), boundary)
    };

    body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("Authorization", token)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn ids(json: &Value) -> Vec<String> {
    json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap().to_string())
        .collect()
}

#[sqlx::test]
async fn nsfw_excluded_from_lists_unless_grab_nsfw(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let tok = bearer(TEST_USER);

    let sfw = upload_tagged(&app, &tok, &make_gif(40, 40, 1), "cat", false, "s1").await;
    let nsfw = upload_tagged(&app, &tok, &make_gif(41, 41, 1), "cat", true, "s2").await;

    // recent: default excludes NSFW
    let def = body_json(
        app.clone()
            .oneshot(
                Request::get("/gifs/recent")
                    .header("Authorization", &tok)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let def_ids = ids(&def);
    assert!(def_ids.contains(&sfw));
    assert!(!def_ids.contains(&nsfw));

    // recent: grab_nsfw=true includes it
    let all = body_json(
        app.clone()
            .oneshot(
                Request::get("/gifs/recent?grab_nsfw=true")
                    .header("Authorization", &tok)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let all_ids = ids(&all);
    assert!(all_ids.contains(&sfw));
    assert!(all_ids.contains(&nsfw));

    // tag search: default excludes NSFW, grab_nsfw includes it
    let s_def = body_json(
        app.clone()
            .oneshot(
                Request::get("/gifs/search?q=cat")
                    .header("Authorization", &tok)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert!(!ids(&s_def).contains(&nsfw));

    let s_all = body_json(
        app.oneshot(
            Request::get("/gifs/search?q=cat&grab_nsfw=true")
                .header("Authorization", &tok)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body(),
    )
    .await;
    assert!(ids(&s_all).contains(&nsfw));
}

#[sqlx::test]
async fn own_nsfw_gif_still_excluded_by_default(pool: PgPool) {
    // The filter is unconditional: even the uploader doesn't see their own
    // NSFW GIF in a default (grab_nsfw=false) listing.
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let tok = bearer(TEST_USER);

    let nsfw = upload_tagged(&app, &tok, &make_gif(40, 40, 1), "x", true, "o1").await;

    let def = body_json(
        app.oneshot(
            Request::get("/gifs/recent?mine=true")
                .header("Authorization", &tok)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body(),
    )
    .await;
    assert!(!ids(&def).contains(&nsfw));
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn health_is_public_and_ok(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let res = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Favorites
// ---------------------------------------------------------------------------

async fn favorite(app: &axum::Router, token: &str, id: &str, method: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/gifs/{}/favorite", id))
                .header("Authorization", token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[sqlx::test]
async fn favorite_add_list_remove(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let tok = bearer(TEST_USER);
    let id = upload_gif(&app, &tok, &make_gif(40, 40, 1), "f1").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_eq!(favorite(&app, &tok, &id, "PUT").await, StatusCode::NO_CONTENT);
    // idempotent
    assert_eq!(favorite(&app, &tok, &id, "PUT").await, StatusCode::NO_CONTENT);

    let listed = body_json(
        app.clone()
            .oneshot(
                Request::get("/gifs/favorites")
                    .header("Authorization", &tok)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert!(ids(&listed).contains(&id));

    assert_eq!(favorite(&app, &tok, &id, "DELETE").await, StatusCode::NO_CONTENT);

    let after = body_json(
        app.oneshot(
            Request::get("/gifs/favorites")
                .header("Authorization", &tok)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body(),
    )
    .await;
    assert!(!ids(&after).contains(&id));
}

#[sqlx::test]
async fn cannot_favorite_others_private_gif(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);

    let res = app
        .clone()
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", bearer(TEST_USER))
                .header("Content-Type", "multipart/form-data; boundary=fp")
                .body(Body::from(multipart_vis(&make_gif(40, 40, 1), "private", "fp")))
                .unwrap(),
        )
        .await
        .unwrap();
    let id = body_json(res.into_body()).await["id"].as_str().unwrap().to_string();

    assert_eq!(
        favorite(&app, &bearer(TEST_OTHER), &id, "PUT").await,
        StatusCode::NOT_FOUND
    );
}

// ---------------------------------------------------------------------------
// Hidden
// ---------------------------------------------------------------------------

async fn hide(app: &axum::Router, token: &str, id: &str, method: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/gifs/{}/hide", id))
                .header("Authorization", token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn list_ids(app: &axum::Router, token: &str, uri: &str) -> Vec<String> {
    ids(&body_json(
        app.clone()
            .oneshot(
                Request::get(uri)
                    .header("Authorization", token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await)
}

#[sqlx::test]
async fn hidden_excluded_from_browse_unless_grab_hidden(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let tok = bearer(TEST_USER);
    let id = upload_gif(&app, &tok, &make_gif(40, 40, 1), "hd1").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Visible everywhere before hiding.
    assert!(list_ids(&app, &tok, "/gifs/search").await.contains(&id));
    assert!(list_ids(&app, &tok, "/gifs/recent").await.contains(&id));
    assert!(list_ids(&app, &tok, "/gifs/featured").await.contains(&id));

    assert_eq!(hide(&app, &tok, &id, "PUT").await, StatusCode::NO_CONTENT);
    // idempotent
    assert_eq!(hide(&app, &tok, &id, "PUT").await, StatusCode::NO_CONTENT);

    // Gone from every browse surface...
    assert!(!list_ids(&app, &tok, "/gifs/search").await.contains(&id));
    assert!(!list_ids(&app, &tok, "/gifs/recent").await.contains(&id));
    assert!(!list_ids(&app, &tok, "/gifs/featured").await.contains(&id));

    // ...but reachable when grab_hidden is set, and via the hidden list.
    assert!(list_ids(&app, &tok, "/gifs/search?grab_hidden=true").await.contains(&id));
    assert!(list_ids(&app, &tok, "/gifs/recent?grab_hidden=true").await.contains(&id));
    assert!(list_ids(&app, &tok, "/gifs/hidden").await.contains(&id));

    // Still reachable by direct id.
    assert_eq!(
        app.clone()
            .oneshot(
                Request::get(format!("/gifs/{}", id))
                    .header("Authorization", &tok)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    assert_eq!(hide(&app, &tok, &id, "DELETE").await, StatusCode::NO_CONTENT);
    assert!(list_ids(&app, &tok, "/gifs/search").await.contains(&id));
    assert!(!list_ids(&app, &tok, "/gifs/hidden").await.contains(&id));
}

#[sqlx::test]
async fn hiding_is_per_user(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let id = upload_gif(&app, &bearer(TEST_USER), &make_gif(40, 40, 1), "hd2").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_eq!(
        hide(&app, &bearer(TEST_USER), &id, "PUT").await,
        StatusCode::NO_CONTENT
    );

    // The other user never hid it, so they still see it.
    assert!(list_ids(&app, &bearer(TEST_OTHER), "/gifs/search").await.contains(&id));
    assert!(!list_ids(&app, &bearer(TEST_USER), "/gifs/search").await.contains(&id));
}

#[sqlx::test]
async fn cannot_hide_others_private_gif(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);

    let res = app
        .clone()
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", bearer(TEST_USER))
                .header("Content-Type", "multipart/form-data; boundary=fp")
                .body(Body::from(multipart_vis(&make_gif(40, 40, 1), "private", "fp")))
                .unwrap(),
        )
        .await
        .unwrap();
    let id = body_json(res.into_body()).await["id"].as_str().unwrap().to_string();

    assert_eq!(
        hide(&app, &bearer(TEST_OTHER), &id, "PUT").await,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn history_lists_recently_selected(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let tok = bearer(TEST_USER);
    let id = upload_gif(&app, &tok, &make_gif(40, 40, 1), "h1").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Not in history until selected.
    let before = body_json(
        app.clone()
            .oneshot(
                Request::get("/gifs/history")
                    .header("Authorization", &tok)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert!(!ids(&before).contains(&id));

    let sel = app
        .clone()
        .oneshot(
            Request::post(format!("/gifs/{}/select", id))
                .header("Authorization", &tok)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(sel.status(), StatusCode::NO_CONTENT);

    let after = body_json(
        app.oneshot(
            Request::get("/gifs/history")
                .header("Authorization", &tok)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body(),
    )
    .await;
    assert!(ids(&after).contains(&id));
}

// ---------------------------------------------------------------------------
// Suggest excludes private/NSFW tags
// ---------------------------------------------------------------------------

async fn suggest_tags_req(app: &axum::Router, tok: &str, qs: &str) -> Vec<String> {
    let json = body_json(
        app.clone()
            .oneshot(
                Request::get(format!("/gifs/tags/suggest?{}", qs))
                    .header("Authorization", tok)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    json["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

#[sqlx::test]
async fn suggest_excludes_private_and_nsfw_tags(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let tok = bearer(TEST_USER);

    // shared, sfw -> sugalpha
    upload_tagged(&app, &tok, &make_gif(40, 40, 1), "sugalpha", false, "z1").await;
    // shared, nsfw -> sugbeta
    upload_tagged(&app, &tok, &make_gif(41, 41, 1), "sugbeta", true, "z2").await;
    // private, sfw -> suggamma
    app.clone()
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", &tok)
                .header("Content-Type", "multipart/form-data; boundary=z3")
                .body(Body::from({
                    let mut b = Vec::new();
                    b.extend_from_slice(b"--z3\r\nContent-Disposition: form-data; name=\"file\"; filename=\"g.gif\"\r\nContent-Type: image/gif\r\n\r\n");
                    b.extend_from_slice(&make_gif(42, 42, 1));
                    b.extend_from_slice(b"\r\n--z3\r\nContent-Disposition: form-data; name=\"tags\"\r\n\r\nsuggamma\r\n");
                    b.extend_from_slice(b"--z3\r\nContent-Disposition: form-data; name=\"visibility\"\r\n\r\nprivate\r\n--z3--\r\n");
                    b
                }))
                .unwrap(),
        )
        .await
        .unwrap();

    let def = suggest_tags_req(&app, &tok, "q=sug").await;
    assert!(def.contains(&"sugalpha".to_string()));
    assert!(!def.contains(&"sugbeta".to_string()), "nsfw tag leaked: {def:?}");
    assert!(!def.contains(&"suggamma".to_string()), "private tag leaked: {def:?}");

    let with_nsfw = suggest_tags_req(&app, &tok, "q=sug&grab_nsfw=true").await;
    assert!(with_nsfw.contains(&"sugbeta".to_string()));
    assert!(!with_nsfw.contains(&"suggamma".to_string()), "private tag leaked: {with_nsfw:?}");
}

// ---------------------------------------------------------------------------
// Per-user quota + per-uploader dedup + auth rate limit
// ---------------------------------------------------------------------------

fn make_state_user_cap(pool: PgPool, per_user: u64) -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config(dir.path().to_str().unwrap().to_string(), u64::MAX);
    cfg.per_user_storage_bytes = per_user;
    let state = AppState {
        pool,
        config: Arc::new(cfg),
        http: reqwest::Client::new(),
    };
    (state, dir)
}

#[sqlx::test]
async fn per_user_quota_exceeded_returns_507(pool: PgPool) {
    // Global cap unlimited, per-user cap 1 byte -> first upload rejected.
    let (state, _dir) = make_state_user_cap(pool, 1);
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", bearer(TEST_USER))
                .header("Content-Type", "multipart/form-data; boundary=pq")
                .body(Body::from(multipart_body(&make_gif(50, 50, 2), None, "pq")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::INSUFFICIENT_STORAGE);
}

#[sqlx::test]
async fn identical_file_from_different_users_is_not_deduped(pool: PgPool) {
    // L1: dedup is per-uploader. The same bytes uploaded by another user
    // create a distinct GIF (no 403 oracle, no shared row).
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let gif = make_gif(48, 48, 1);

    let a = upload_gif(&app, &bearer(TEST_USER), &gif, "d1").await;
    let a_id = a["id"].as_str().unwrap().to_string();
    assert_eq!(a["uploader_id"], TEST_USER);

    let res = app
        .clone()
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", bearer(TEST_OTHER))
                .header("Content-Type", "multipart/form-data; boundary=d2")
                .body(Body::from(multipart_body(&gif, None, "d2")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED); // not 200-dup, not 403
    let b = body_json(res.into_body()).await;
    assert_ne!(b["id"].as_str().unwrap(), a_id);
    assert_eq!(b["uploader_id"], TEST_OTHER);

    // Same user re-uploading the same bytes still dedups (200).
    let again = app
        .oneshot(
            Request::post("/gifs")
                .header("Authorization", bearer(TEST_USER))
                .header("Content-Type", "multipart/form-data; boundary=d3")
                .body(Body::from(multipart_body(&gif, None, "d3")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::OK);
    assert_eq!(body_json(again.into_body()).await["id"].as_str().unwrap(), a_id);
}

#[sqlx::test]
async fn auth_matrix_is_rate_limited(pool: PgPool) {
    // /auth/matrix has two layered limiters:
    //   * per-IP (burst 5) — one IP gets throttled quickly
    //   * global  (burst 20) — caps total backend load
    // We verify both: a single IP hits 429 within its per-IP burst, and
    // rotating IPs eventually hit the global cap.
    let (state, _dir) = make_state(pool);
    let app = create_router(state);

    let send = |ip: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(
                Request::post("/auth/matrix")
                    .header("Content-Type", "application/json")
                    .header("X-Forwarded-For", ip)
                    .body(Body::from(
                        r#"{"access_token":"x","matrix_server_name":"test.server"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };

    // Per-IP limit: single IP hammered should 429 within ~burst_size requests.
    let mut saw_per_ip_429 = false;
    for _ in 0..15 {
        if send("1.2.3.4").await.status() == StatusCode::TOO_MANY_REQUESTS {
            saw_per_ip_429 = true;
            break;
        }
    }
    assert!(saw_per_ip_429, "expected per-IP 429 within burst window");

    // Global limit: rotating IPs each have their own per-IP bucket, but
    // the shared global bucket (burst 20) should still kick in.
    let mut saw_global_429 = false;
    for i in 0..40u8 {
        // synthesize a unique IP per request to dodge the per-IP limiter
        let ip: &'static str = Box::leak(format!("10.0.0.{}", i).into_boxed_str());
        if send(ip).await.status() == StatusCode::TOO_MANY_REQUESTS {
            saw_global_429 = true;
            break;
        }
    }
    assert!(saw_global_429, "expected global 429 once burst exhausted across many IPs");
}

#[sqlx::test]
async fn repeated_select_counts_uses_once_per_user(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);
    let id = upload_gif(&app, &bearer(TEST_USER), &make_gif(40, 40, 1), "u1").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let select = |tok: String, app: axum::Router, id: String| async move {
        app.oneshot(
            Request::post(format!("/gifs/{}/select", id))
                .header("Authorization", tok)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    };

    // Same user selecting three times only counts once (within 24h).
    for _ in 0..3 {
        assert_eq!(
            select(bearer(TEST_USER), app.clone(), id.clone()).await,
            StatusCode::NO_CONTENT
        );
    }
    // A different user's selection counts separately.
    assert_eq!(
        select(bearer(TEST_OTHER), app.clone(), id.clone()).await,
        StatusCode::NO_CONTENT
    );

    let json = body_json(
        app.oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("Authorization", bearer(TEST_USER))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body(),
    )
    .await;
    assert_eq!(json["uses"], 2); // TEST_USER once + TEST_OTHER once
}

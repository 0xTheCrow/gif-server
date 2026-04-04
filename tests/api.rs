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
use tokio::sync::RwLock;
use tower::ServiceExt;

use gif_server::{config::Config, AppState, create_router};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_state(pool: PgPool) -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        database_url: String::new(),
        storage_path: dir.path().to_str().unwrap().to_string(),
        host: "127.0.0.1".to_string(),
        port: 8847,
        base_url: "http://localhost:8847".to_string(),
    };
    let state = AppState {
        pool,
        config: Arc::new(config),
        key_cache: Arc::new(RwLock::new(None)),
    };
    (state, dir)
}

/// Insert a hashed API key and return the raw key string.
async fn insert_api_key(pool: &PgPool) -> String {
    use argon2::{
        password_hash::{rand_core::OsRng, SaltString},
        Argon2, PasswordHasher,
    };
    let key = "test-key-abc123";
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(key.as_bytes(), &salt)
        .unwrap()
        .to_string();
    sqlx::query("INSERT INTO api_keys (key_hash, name) VALUES ($1, 'test')")
        .bind(&hash)
        .execute(pool)
        .await
        .unwrap();
    key.to_string()
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
async fn missing_api_key_returns_401(pool: PgPool) {
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
async fn invalid_api_key_returns_401(pool: PgPool) {
    let (state, _dir) = make_state(pool);
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::get("/gifs/search")
                .header("X-API-Key", "wrong-key")
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::get("/gifs/search")
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let gif = make_gif(100, 80, 3);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, Some("nature,birds"), boundary);

    let res = app
        .oneshot(
            Request::post("/gifs")
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, None, boundary);

    let res1 = app
        .clone()
        .oneshot(
            Request::post("/gifs")
                .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let boundary = "testboundary123";
    let body = multipart_body(b"this is not a gif", None, boundary);

    let res = app
        .oneshot(
            Request::post("/gifs")
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    // Send 30 tags, expect only 20 stored
    let many_tags = (0..30).map(|i| format!("tag{}", i)).collect::<Vec<_>>().join(",");
    let body = multipart_body(&gif, Some(&many_tags), boundary);

    let res = app
        .oneshot(
            Request::post("/gifs")
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, Some("cats,funny"), boundary);
    app.clone()
        .oneshot(
            Request::post("/gifs")
                .header("X-API-Key", &key)
                .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    let res = app
        .oneshot(
            Request::get("/gifs/search?q=cats")
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::get("/gifs/search?q=doesnotexist")
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    // Upload
    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, None, boundary);
    let upload_res = app
        .clone()
        .oneshot(
            Request::post("/gifs")
                .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, Some("old-tag"), boundary);
    let upload_json = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"tags":["new-tag"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    let get_res = app
        .oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, None, boundary);
    let upload_json = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del_res.status(), StatusCode::NO_CONTENT);

    let get_res = app
        .oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, Some("cartoon,castle,cat"), boundary);
    app.clone()
        .oneshot(
            Request::post("/gifs")
                .header("X-API-Key", &key)
                .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    let res = app
        .oneshot(
            Request::get("/gifs/tags/suggest?q=ca")
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, Some("test"), boundary);
    let upload_json = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::get("/gifs/00000000-0000-0000-0000-000000000000")
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, None, boundary);
    let id = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let body = multipart_body(&gif, None, boundary);
    let id = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
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
                    .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
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
                    .header("X-API-Key", &key)
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
                    .header("X-API-Key", &key)
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
                    .header("X-API-Key", &key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    }

    let res = app
        .oneshot(
            Request::get("/gifs/featured")
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
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
                        .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let id = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"tags":["added"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    let json = body_json(
        app.oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let id = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"tags":["remove"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    let json = body_json(
        app.oneshot(
            Request::get(format!("/gifs/{}", id))
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::post("/gifs/00000000-0000-0000-0000-000000000000/select")
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/gifs/00000000-0000-0000-0000-000000000000")
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let gif = make_gif(50, 50, 1);
    let boundary = "testboundary123";
    let upload_json = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("X-API-Key", &key)
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
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let res = app
        .oneshot(
            Request::get("/gifs/tags/suggest?q=ca%25te%5F")
                .header("X-API-Key", &key)
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
    let key = insert_api_key(&pool).await;
    let app = create_router(state);

    let boundary = "testboundary123";

    // GIF A: only "cat"
    let gif_a = make_gif(40, 40, 1);
    let _id_a = body_json(
        app.clone()
            .oneshot(
                Request::post("/gifs")
                    .header("X-API-Key", &key)
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
                    .header("X-API-Key", &key)
                    .header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
                    .body(Body::from(multipart_body(&gif_b, Some("cat,funny"), boundary)))
                    .unwrap(),
            )
            .await.unwrap().into_body(),
    ).await["id"].as_str().unwrap().to_string();

    let res = app
        .oneshot(
            Request::get("/gifs/search?q=cat+funny")
                .header("X-API-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let json = body_json(res.into_body()).await;
    let first_id = json["results"][0]["id"].as_str().unwrap();
    assert_eq!(first_id, id_b);
}

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn make_app() -> Router {
    make_app_with_store(lingrep_web::RequestStore::default())
}

fn make_app_with_store(store: lingrep_web::RequestStore) -> Router {
    Router::new()
        .merge(lingrep_web::handlers::index::routes())
        .nest(
            "/api",
            Router::new()
                .merge(lingrep_web::handlers::search::routes())
                .merge(lingrep_web::handlers::stream::routes())
                .merge(lingrep_web::handlers::pattern_detail::routes()),
        )
        .with_state(store)
}

#[tokio::test]
async fn test_full_flow() {
    let app = make_app();

    let response = app
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let text = "pattern=[upos=VERB].&dict_path=/tmp/dict.dic&corpus_path=/tmp/corpus.jsonl";
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search/test-tab")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(Body::from(text))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("sse-connect"));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/stream/test-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_error_handling() {
    let app = make_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/pattern-detail/nonexistent/1/0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("not found") || html.contains("error"));
}

#[tokio::test]
async fn test_index_page_has_htmx_attrs() {
    let app = make_app();

    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();

    assert!(
        html.contains("hx-post=\"api/search/"),
        "should have hx-post"
    );
    assert!(
        html.contains("hx-target=\"#search-status\""),
        "should have hx-target"
    );
    assert!(
        html.contains("hx-swap=\"innerHTML\""),
        "should have hx-swap"
    );
}

#[tokio::test]
async fn test_search_returns_sse_container() {
    let app = make_app();

    let text = "pattern=[upos=VERB].&dict_path=/tmp/dict.dic&corpus_path=/tmp/corpus.jsonl";
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search/test-tab")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(Body::from(text))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();

    assert!(html.contains("id=\"search-stream\""));
    assert!(!html.contains("Showing previous results"));
    assert!(
        html.contains("sse-connect=\"api/stream/"),
        "should connect to stream"
    );
    assert!(
        html.contains("sse-swap=\"progress,result,error\""),
        "should subscribe to the event names emitted by the server"
    );
    assert!(
        html.contains("hx-swap=\"innerHTML\""),
        "each progress or terminal event should replace the prior status"
    );
}

#[tokio::test]
async fn test_sse_event_format() {
    let app = make_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/stream/test-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response.headers().get("content-type").unwrap();
    assert!(content_type.to_str().unwrap().contains("text/event-stream"));

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let sse_data = String::from_utf8(body.to_vec()).unwrap();

    assert!(sse_data.contains("event:"), "should have event name");
    assert!(sse_data.contains("data:"), "should have data");
    // Will return error if no channel exists, or progress/complete if channel exists
    assert!(
        sse_data.contains("error")
            || sse_data.contains("progress")
            || sse_data.contains("complete"),
        "should have error, progress or complete event"
    );
}

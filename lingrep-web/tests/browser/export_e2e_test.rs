#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::string_slice
)]

use askama::Template;
use axum::{
    Router,
    extract::Path,
    http::{StatusCode, header},
    response::{Html, IntoResponse},
    routing::get,
};
use serde_json::json;
use std::time::Duration;
use thirtyfour::prelude::*;

async fn page(Path(status): Path<u16>) -> Html<String> {
    let workbench = status.to_string();
    let explorer = lingrep_web::templates::ResultsExplorer {
        search_scope: None,
        excluded_sentences: 0,
        cli_arguments: "",
        workbench_id: &workbench,
        revision: 5,
        results: &[],
        has_results: true,
        grouping_fields: &[],
        default_grouping_field: "",
        coverage: None,
        section_analysis: None,
    }
    .render()
    .unwrap();
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><link rel=\"stylesheet\" href=\"/assets/app.css\"></head><body>{explorer}</body></html>"
    ))
}

async fn export(Path((status, _revision)): Path<(u16, u64)>) -> axum::response::Response {
    if status == 200 {
        (
            [
                (
                    header::CONTENT_TYPE,
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                ),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"concordance-5.xlsx\"",
                ),
            ],
            "workbook fixture",
        )
            .into_response()
    } else {
        (StatusCode::from_u16(status).unwrap(), Html(format!("<div class=\"alert alert-error\">Export rejected ({status}). Run the search again.</div>"))).into_response()
    }
}

#[tokio::test]
async fn export_links_display_rejections_and_save_without_extra_tabs() -> WebDriverResult<()> {
    let endpoint = if let Ok(endpoint) = std::env::var("LINGREP_WEB_EXPORT_WEBDRIVER_URL") {
        endpoint
    } else if let Some(endpoint) = crate::chromedriver_url().await {
        endpoint
    } else {
        return Ok(());
    };
    let directory = tempfile::tempdir().unwrap();
    let browser = std::env::var("LINGREP_WEB_EXPORT_BROWSER").unwrap_or_else(|_| "chrome".into());
    let caps = match browser.as_str() {
        "chrome" => json!({"browserName":"chrome", "goog:chromeOptions": {
            "args":["--headless", "--no-sandbox", "--disable-dev-shm-usage"],
            "prefs":{"download.default_directory":directory.path(), "download.prompt_for_download":false}
        }}),
        "firefox" => json!({"browserName":"firefox", "moz:firefoxOptions": {
            "binary":std::env::var("FIREFOX_BIN").expect("FIREFOX_BIN for the Firefox download check"),
            "args":["-headless"], "prefs":{"browser.download.dir":directory.path(),
                "browser.download.folderList":2, "browser.download.useDownloadDir":true,
                "browser.helperApps.neverAsk.saveToDisk":"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"}
        }}),
        _ => panic!("unsupported local export test browser: {browser}"),
    };
    let driver = crate::session(&endpoint, caps.as_object().unwrap().clone()).await?;
    driver.set_window_rect(0, 0, 1100, 800).await?;
    let app = Router::new()
        .route("/{status}", get(page))
        .route("/api/export/{status}/{revision}", get(export))
        .route(
            "/assets/app.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("../../assets/app.css"),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let result: WebDriverResult<()> = async {
        driver.goto(format!("{base}/200")).await?;
        let original = driver.window().await?;
        driver
            .find(By::Css("a.export-download"))
            .await?
            .click()
            .await?;
        let file = directory.path().join("concordance-5.xlsx");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !file.exists() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if !file.exists() {
            return Err(WebDriverError::Timeout("workbook download".into()));
        }
        assert_eq!(std::fs::read(&file).unwrap(), b"workbook fixture");
        assert_eq!(
            driver.windows().await?.len(),
            1,
            "successful export left a blank tab"
        );
        for status in [404, 409, 500, 503] {
            driver.goto(format!("{base}/{status}")).await?;
            driver
                .find(By::Css("a.export-download"))
                .await?
                .click()
                .await?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            let window = loop {
                if let Some(window) = driver
                    .windows()
                    .await?
                    .into_iter()
                    .find(|window| window != &original)
                {
                    break window;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(WebDriverError::Timeout(format!(
                        "visible export rejection {status}"
                    )));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            };
            driver.switch_to_window(window).await?;
            let message = driver
                .query(By::Css(".alert-error"))
                .wait(Duration::from_secs(3), Duration::from_millis(25))
                .first()
                .await?
                .text()
                .await?;
            assert!(
                message.contains(&format!("Export rejected ({status})")),
                "{message}"
            );
            driver.close_window().await?;
            driver.switch_to_window(original.clone()).await?;
        }
        Ok(())
    }
    .await;
    let quit = driver.quit().await;
    server.abort();
    result?;
    quit
}

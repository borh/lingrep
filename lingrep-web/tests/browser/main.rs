//! Browser-driven lingrep-web tests. Require chromedriver and a Chrome or
//! Chromium of the same major version.
//! Run via scripts/run_lingrep_web_layout_e2e.sh, which pins --test-threads=1.

mod compare_e2e_test;
mod corpus_context_e2e_test;
mod export_e2e_test;
mod frequency_e2e_test;
mod layout_e2e_test;
mod query_library_e2e_test;
mod reader_e2e_test;
mod sample_e2e_test;

pub(crate) const MOD: thirtyfour::Key = if cfg!(target_os = "macos") {
    thirtyfour::Key::Meta
} else {
    thirtyfour::Key::Control
};

async fn chromedriver_url() -> Option<String> {
    let required = std::env::var_os("LINGREP_WEB_LAYOUT_REQUIRE_DRIVER").is_some();
    let Ok(url) = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL") else {
        assert!(
            !required,
            "LINGREP_WEB_LAYOUT_REQUIRE_DRIVER is set but LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL is not"
        );
        eprintln!("SKIP: LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL is unset");
        return None;
    };
    let address = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(&url);
    if tokio::net::TcpStream::connect(address).await.is_err() {
        assert!(
            !required,
            "LINGREP_WEB_LAYOUT_REQUIRE_DRIVER is set but no Chromedriver at {url}"
        );
        eprintln!("SKIP: Chromedriver is unavailable at {url}");
        return None;
    }
    Some(url)
}

pub(crate) async fn session(
    driver_url: impl Into<String>,
    capabilities: impl Into<thirtyfour::Capabilities>,
) -> thirtyfour::prelude::WebDriverResult<thirtyfour::WebDriver> {
    let client = thirtyfour_http::Client::builder()
        .pool_max_idle_per_host(0)
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("the WebDriver HTTP client builds");
    thirtyfour::WebDriver::builder(driver_url, capabilities)
        .client(client)
        .await
}

pub(crate) async fn resize(
    driver: &thirtyfour::WebDriver,
    width: u32,
    height: u32,
) -> thirtyfour::prelude::WebDriverResult<()> {
    let tools = driver.cdp();
    tools
        .send_raw(
            "Emulation.clearDeviceMetricsOverride",
            serde_json::json!({}),
        )
        .await?;
    driver.set_window_rect(0, 0, width, height).await?;
    let seen: u32 = driver
        .execute("return innerWidth", Vec::new())
        .await?
        .convert()?;
    if seen != width {
        tools
            .send_raw(
                "Emulation.setDeviceMetricsOverride",
                serde_json::json!({"width": width, "height": 0, "deviceScaleFactor": 0, "mobile": false}),
            )
            .await?;
    }
    Ok(())
}

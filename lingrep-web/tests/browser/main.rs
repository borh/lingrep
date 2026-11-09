//! Browser-driven lingrep-web tests. Require chromium + chromedriver.
//! Run via scripts/run_lingrep_web_layout_e2e.sh, which pins --test-threads=1.

mod compare_e2e_test;
mod corpus_context_e2e_test;
mod frequency_e2e_test;
mod layout_e2e_test;
mod query_library_e2e_test;

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

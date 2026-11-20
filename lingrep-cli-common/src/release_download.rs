pub const DEFAULT_DOWNLOAD_BASE: &str = "https://github.com/borh/lingrep/releases/download";

/// Rejects HTTP error statuses and removes credentials, queries and fragments
/// from error URLs.
pub fn get(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<reqwest::blocking::Response, reqwest::Error> {
    client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|mut error| {
            if let Some(url) = error.url_mut() {
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.set_query(None);
                url.set_fragment(None);
            }
            error
        })
}

//! How Kumi's HTTP clients are made, in one place, so they all trust certificates and go through proxies alike
//! (sign-in, updates, model calls, downloads). Each client begins at [`client`] and adds its own timeouts, redirects
//! and user agent. [`direct`] is for the web tool's guarded lookups, which a proxy would get around.

pub mod proxy;
mod trust;

use std::sync::Arc;

/// The beginning of every HTTP client Kumi makes: its trust (the certificates Kumi carries, then this computer's own)
/// and its proxies (the environment's or the system's, never for this computer itself).
pub fn client() -> reqwest::ClientBuilder {
    client_through(proxy::current)
}

/// [`client`] with proxies from `settings` (asked as each connection is made) instead of the environment's or the
/// system's.
pub fn client_through(settings: impl Fn() -> Arc<proxy::Settings> + Send + Sync + 'static) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .use_preconfigured_tls(trust::config(false))
        .proxy(reqwest::Proxy::custom(move |url| settings().proxy_for(url)))
}

/// A client's beginning that never goes through a proxy. The web tool looks each name up itself and connects only
/// to public addresses: a proxy would look the name up again, where Kumi can't check it. Nor does its certificate check
/// fetch anything (an issuer a certificate names could be any address): it reads the computer's list of trusted roots.
pub fn direct() -> reqwest::ClientBuilder {
    reqwest::Client::builder().use_preconfigured_tls(trust::guarded_config(false)).no_proxy()
}

/// [`direct`], speaking HTTP/1.1 only.
pub fn direct_http1() -> reqwest::ClientBuilder {
    reqwest::Client::builder().use_preconfigured_tls(trust::guarded_config(true)).http1_only().no_proxy()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// Every client is made here: one made elsewhere would trust and proxy its own way. (Tests may make their own.)
    #[test]
    fn every_client_begins_here() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let crates = here.parent().unwrap();
        let mut sources = vec![crates.to_path_buf()];
        let mut elsewhere = Vec::new();
        while let Some(path) = sources.pop() {
            if path.is_dir() {
                if path != here.join("src").join("net") && path.file_name().is_some_and(|name| name != "tests" && name != "target") {
                    sources.extend(std::fs::read_dir(&path).unwrap().map(|entry| entry.unwrap().path()));
                }
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                let code = text.split("#[cfg(test)]").next().unwrap_or("");
                for made in ["Client::builder()", "Client::new()", "ClientBuilder::new()", "reqwest::get(", "reqwest::blocking"] {
                    if code.contains(made) {
                        elsewhere.push(format!("{}: {made}", path.strip_prefix(crates).unwrap().display()));
                    }
                }
            }
        }
        assert!(elsewhere.is_empty(), "begin these at kumi_runtime::net::client (or direct): {elsewhere:#?}");
    }
}

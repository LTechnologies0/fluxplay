//! Where each server of the account really serves the file.
//!
//! Xtream panels (XUI.one and alikes) answer a media request with a redirect
//! to a CDN node, carrying a token bound to the title and to that node. The
//! panel counts tokens, not TCP connections, against `max_connections`: one
//! token too many and it revokes an older one a few seconds later, transfers
//! in flight included, while any number of connections may share a token.
//! So every connection of a download reuses the redirect target of its
//! server, and only one at a time asks the panel again, once the target
//! stops answering.
//!
//! A token revoked soon after it was issued means another stream of the
//! account (a player elsewhere) keeps taking it back: renewing at once would
//! cut that viewer again and again, so renewals then wait, longer each time.

use std::time::{Duration, Instant};

use reqwest::{Client, RequestBuilder, Response, Url};
use tokio::sync::Mutex;

/// A token revoked younger than this was taken back by another stream.
const CONTESTED: Duration = Duration::from_secs(60);
const MAX_RENEW_WAIT: Duration = Duration::from_secs(5 * 60);

pub(crate) struct Links {
    servers: Vec<Server>,
    /// First wait before renewing a contested token.
    renew_wait: Duration,
}

struct Server {
    panel: String,
    panel_url: Option<Url>,
    token: Mutex<Token>,
}

#[derive(Default)]
struct Token {
    /// Where the panel last redirected to (token included). Never logged.
    target: Option<Url>,
    issued: Option<Instant>,
    /// Contested renewals in a row.
    contested: u32,
}

/// A server's answer. `fresh`: it went through the panel, so a refusal is the
/// account's; a reused target that stopped answering is renewed instead.
pub(crate) struct Sent {
    pub resp: Response,
    pub fresh: bool,
}

impl Links {
    pub(crate) fn new(urls: Vec<String>, renew_wait: Duration) -> Self {
        Self {
            servers: urls
                .into_iter()
                .map(|panel| Server {
                    panel_url: Url::parse(&panel).ok(),
                    panel,
                    token: Mutex::new(Token::default()),
                })
                .collect(),
            renew_wait,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.servers.len()
    }

    pub(crate) fn first(&self) -> Option<&str> {
        self.servers.first().map(|s| s.panel.as_str())
    }

    /// GET server `idx` with the request `build` adds (`Range`…): its known
    /// target, else through the panel.
    pub(crate) async fn send(
        &self,
        client: &Client,
        idx: usize,
        build: impl Fn(RequestBuilder) -> RequestBuilder,
    ) -> Result<Sent, reqwest::Error> {
        let server = &self.servers[idx];
        let known = server.token.lock().await.target.clone();
        if let Some(url) = known {
            match build(client.get(url.clone())).send().await {
                Ok(resp) if resp.status().is_success() => return Ok(Sent { resp, fresh: false }),
                // Revoked or expired token, or the node is gone.
                Ok(_) => {}
                Err(e) if e.is_connect() => {}
                Err(e) => return Err(e),
            }
            let mut token = server.token.lock().await;
            if token.target.as_ref() == Some(&url) {
                token.target = None;
            }
        }
        let mut token = server.token.lock().await;
        if let Some(url) = token.target.clone() {
            // Another connection renewed it meanwhile.
            drop(token);
            let resp = build(client.get(url)).send().await?;
            return Ok(Sent { resp, fresh: false });
        }
        if let Some(issued) = token.issued {
            token.contested = if issued.elapsed() < CONTESTED {
                token.contested + 1
            } else {
                0
            };
            let wait = renew_wait(self.renew_wait, token.contested);
            if !wait.is_zero() {
                tracing::info!(server = idx, wait_s = wait.as_secs(), "downloads: stream token taken back by another player, waiting");
                tokio::time::sleep(wait).await;
            }
        }
        let resp = build(client.get(&server.panel)).send().await?;
        let redirected = server.panel_url.as_ref() != Some(resp.url());
        if resp.status().is_success() && redirected {
            tracing::info!(server = idx, "downloads: new stream token from the panel");
            token.target = Some(resp.url().clone());
            token.issued = Some(Instant::now());
        }
        Ok(Sent { resp, fresh: true })
    }
}

/// Wait before the `contested`th renewal in a row of a token taken back early.
fn renew_wait(first: Duration, contested: u32) -> Duration {
    match contested {
        0 => Duration::ZERO,
        n => first.saturating_mul(1 << (n - 1).min(8)).min(MAX_RENEW_WAIT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contested_renewals_back_off() {
        let first = Duration::from_secs(15);
        assert_eq!(renew_wait(first, 0), Duration::ZERO);
        assert_eq!(renew_wait(first, 1), first);
        assert_eq!(renew_wait(first, 3), Duration::from_secs(60));
        assert_eq!(renew_wait(first, 20), MAX_RENEW_WAIT);
    }
}

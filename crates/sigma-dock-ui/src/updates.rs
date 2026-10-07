//! Explicit GitHub release checks. Never reads workspace data or authentication.
use reqwest::{blocking::Client, header};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::Read,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COMMIT: &str = env!("SIGMA_DOCK_BUILD_COMMIT");
pub const BUILD_CHANNEL: &str = env!("SIGMA_DOCK_BUILD_CHANNEL");
const RELEASES: &str = "https://api.github.com/repos/SigmaUno/sigma-dock/releases";
const DAY: u64 = 86400;
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum Channel {
    #[default]
    Stable,
    Preview,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct UpdatePreferences {
    pub automatic: bool,
    pub channel: Channel,
    pub next_check: u64,
    pub dismissed: Option<String>,
}
impl UpdatePreferences {
    pub fn due(&self, now: u64) -> bool {
        self.automatic && now >= self.next_check
    }
    pub fn should_notify(&self, version: &str, manual: bool) -> bool {
        manual || self.dismissed.as_deref() != Some(version)
    }
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub state: String,
    pub size: u64,
    pub browser_download_url: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub html_url: String,
    pub body: Option<String>,
    pub assets: Vec<Asset>,
}
#[derive(Clone, Debug)]
pub struct Available {
    pub version: Version,
    pub notes: String,
    pub url: String,
    pub installer: String,
}
#[derive(Clone, Debug)]
pub struct CheckError {
    pub message: String,
    pub retry_after: u64,
}
#[derive(Clone)]
struct CachedPage {
    releases: Vec<Release>,
    etag: Option<String>,
    next: bool,
}
#[derive(Default)]
pub struct Checker {
    pages: HashMap<usize, CachedPage>,
    failures: u32,
    not_before: u64,
}
fn trusted_page(url: &str) -> bool {
    url.starts_with("https://github.com/SigmaUno/sigma-dock/releases/tag/")
}
fn compatible(asset: &Asset, os: &str, arch: &str) -> bool {
    if asset.state != "uploaded"
        || asset.size == 0
        || !asset
            .browser_download_url
            .starts_with("https://github.com/SigmaUno/sigma-dock/releases/download/")
    {
        return false;
    }
    let name = asset.name.to_ascii_lowercase();
    if !name.starts_with("sigmadock-") {
        return false;
    }
    let architecture = match arch {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        _ => return false,
    };
    let has_arch = name.contains(&format!("-{architecture}-"))
        || name.ends_with(&format!("-{architecture}.dmg"))
        || name.contains("-universal-")
        || name.ends_with("-universal.dmg");
    match os {
        "macos" => has_arch && name.ends_with(".dmg"),
        _ => false,
    }
}
pub fn newest(
    releases: &[Release],
    installed: &Version,
    channel: Channel,
    os: &str,
    arch: &str,
) -> Option<Available> {
    releases
        .iter()
        .filter_map(|release| {
            if release.draft || !trusted_page(&release.html_url) {
                return None;
            }
            let version = Version::parse(release.tag_name.strip_prefix('v')?).ok()?;
            if version.cmp_precedence(installed).is_le()
                || (channel == Channel::Stable && (release.prerelease || !version.pre.is_empty()))
            {
                return None;
            }
            let asset = release
                .assets
                .iter()
                .filter(|asset| compatible(asset, os, arch))
                .min_by_key(|asset| !asset.name.contains("universal"))?;
            Some(Available {
                version,
                notes: release
                    .body
                    .as_deref()
                    .unwrap_or("No release notes provided.")
                    .chars()
                    .take(4000)
                    .collect(),
                url: release.html_url.clone(),
                installer: asset.name.clone(),
            })
        })
        .max_by(|left, right| left.version.cmp_precedence(&right.version))
}
impl Checker {
    pub fn check(&mut self, channel: Channel) -> Result<Option<Available>, CheckError> {
        self.check_from(channel, RELEASES)
    }
    fn check_from(
        &mut self,
        channel: Channel,
        base: &str,
    ) -> Result<Option<Available>, CheckError> {
        let current = now();
        if current < self.not_before {
            return Err(CheckError { message: "Update checks are paused after a network error or GitHub rate limit. Try again later.".into(), retry_after: self.not_before - current });
        }
        let result = self.fetch(base).map(|releases| {
            newest(
                &releases,
                &Version::parse(VERSION).expect("Cargo version"),
                channel,
                std::env::consts::OS,
                std::env::consts::ARCH,
            )
        });
        match &result {
            Ok(_) => {
                self.failures = 0;
            }
            Err(error) => {
                self.failures = self.failures.saturating_add(1);
                self.not_before = current
                    .saturating_add(error.retry_after.max(60 * 2u64.pow(self.failures.min(8))));
            }
        }
        result.map_err(|mut error| {
            error.retry_after = self.not_before.saturating_sub(current);
            error
        })
    }
    fn fetch(&mut self, base: &str) -> Result<Vec<Release>, CheckError> {
        let fail = |message: String| CheckError {
            message,
            retry_after: 120,
        };
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("SigmaDock-update-check")
            .build()
            .map_err(|_| fail("Could not initialize update checker".into()))?;
        let mut releases = Vec::new();
        for page in 1..=10 {
            let mut request = client
                .get(base)
                .query(&[("per_page", 100), ("page", page)])
                .header(header::ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28");
            if let Some(etag) = self
                .pages
                .get(&page)
                .and_then(|cached| cached.etag.as_ref())
            {
                request = request.header(header::IF_NONE_MATCH, etag);
            }
            let mut response = request.send().map_err(|_| {
                fail("Could not reach GitHub. Check your connection and try again later.".into())
            })?;
            let status = response.status();
            let cached = if status == reqwest::StatusCode::NOT_MODIFIED {
                self.pages.get(&page).cloned().ok_or_else(|| {
                    fail("GitHub returned a cached response without a local copy".into())
                })?
            } else {
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || (status == reqwest::StatusCode::FORBIDDEN
                        && response
                            .headers()
                            .get("x-ratelimit-remaining")
                            .is_some_and(|v| v == "0"))
                {
                    let retry = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(0);
                    let reset = response
                        .headers()
                        .get("x-ratelimit-reset")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(0)
                        .saturating_sub(now());
                    return Err(CheckError {
                        message: "GitHub rate limit reached. Update checks will retry later."
                            .into(),
                        retry_after: retry.max(reset).clamp(60, DAY),
                    });
                }
                if !status.is_success() {
                    return Err(fail(format!(
                        "GitHub update check returned HTTP {}",
                        status.as_u16()
                    )));
                }
                let etag = response
                    .headers()
                    .get(header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                let next = response
                    .headers()
                    .get(header::LINK)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.contains("rel=\"next\""));
                let mut bytes = Vec::new();
                response
                    .by_ref()
                    .take(2_000_001)
                    .read_to_end(&mut bytes)
                    .map_err(|_| fail("Could not read GitHub releases".into()))?;
                if bytes.len() > 2_000_000 {
                    return Err(fail("GitHub release response is too large".into()));
                }
                let page_releases = serde_json::from_slice(&bytes)
                    .map_err(|_| fail("GitHub returned an invalid release response".into()))?;
                CachedPage {
                    releases: page_releases,
                    etag,
                    next,
                }
            };
            releases.extend(cached.releases.iter().cloned());
            let next = cached.next;
            self.pages.insert(page, cached);
            if !next {
                self.pages.retain(|key, _| *key <= page);
                return Ok(releases);
            }
        }
        Err(fail(
            "Too many release pages; update check could not complete".into(),
        ))
    }
}
pub fn next_check(result: &Result<Option<Available>, CheckError>, now: u64) -> u64 {
    now.saturating_add(match result {
        Ok(_) => DAY,
        Err(error) => error.retry_after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };
    fn release(tag: &str) -> Release {
        Release {
            tag_name: tag.into(),
            draft: false,
            prerelease: false,
            html_url: format!("https://github.com/SigmaUno/sigma-dock/releases/tag/{tag}"),
            body: Some("Release notes".into()),
            assets: vec![Asset {
                name: "SigmaDock-1.2.0-abcdef-universal-test.dmg".into(),
                state: "uploaded".into(),
                size: 123,
                browser_download_url:
                    "https://github.com/SigmaUno/sigma-dock/releases/download/v1.2.0/SigmaDock.dmg"
                        .into(),
            }],
        }
    }
    fn select(releases: &[Release], installed: &str, channel: Channel) -> Option<Available> {
        newest(
            releases,
            &Version::parse(installed).unwrap(),
            channel,
            "macos",
            "aarch64",
        )
    }
    #[test]
    fn semantic_versions_channels_and_snapshot_tags() {
        let mut preview = release("v1.3.0-rc.1");
        preview.prerelease = true;
        let releases = [
            release("v1.2.0"),
            preview,
            release("macos-ffffffffffff"),
            release("v1.10.0"),
        ];
        assert_eq!(
            select(&releases, "1.1.0", Channel::Stable).unwrap().version,
            Version::parse("1.10.0").unwrap()
        );
        assert_eq!(
            select(&releases[..2], "1.2.0", Channel::Preview)
                .unwrap()
                .version,
            Version::parse("1.3.0-rc.1").unwrap()
        );
        assert!(select(&releases[..2], "1.2.0", Channel::Stable).is_none());
        assert!(
            select(
                &[release("v1.2.0+build.100")],
                "1.2.0+build.1",
                Channel::Preview
            )
            .is_none()
        );
        // A snapshot uses its embedded Cargo version; source hashes never enter ordering.
        assert!(
            select(
                &[release("macos-ffffffffffff"), release("v0.1.0")],
                "0.1.0",
                Channel::Preview
            )
            .is_none()
        );
        assert!(select(&[release("v0.1.1")], "0.1.0", Channel::Stable).is_some());
        let mut mismatched = release("v1.4.0");
        mismatched.prerelease = true;
        assert!(select(&[mismatched], "1.0.0", Channel::Stable).is_none());
    }
    #[test]
    fn exclude_missing_incompatible_draft_and_untrusted_downloads() {
        let installed = Version::parse("1.0.0").unwrap();
        for variant in 0..7 {
            let mut candidate = release("v1.2.0");
            match variant {
                0 => candidate.draft = true,
                1 => candidate.assets.clear(),
                2 => candidate.assets[0].name = "SigmaDock-1.2.0-abcdef-x86_64-test.dmg".into(),
                3 => {
                    candidate.assets[0].name =
                        "SigmaDock-1.2.0-abcdef-universal-test.dmg.sha256".into()
                }
                4 => candidate.assets[0].state = "new".into(),
                5 => candidate.assets[0].size = 0,
                _ => {
                    candidate.assets[0].browser_download_url = "https://example.com/app.dmg".into()
                }
            }
            assert!(
                newest(
                    &[candidate],
                    &installed,
                    Channel::Preview,
                    "macos",
                    "aarch64"
                )
                .is_none()
            );
        }
        assert!(
            newest(
                &[release("v1.2.0")],
                &installed,
                Channel::Stable,
                "linux",
                "x86_64"
            )
            .is_none()
        );
    }
    #[test]
    fn opt_in_scheduler_and_dismissal() {
        let mut preferences = UpdatePreferences::default();
        for time in [0, 100, u64::MAX] {
            assert!(!preferences.due(time));
        }
        preferences.automatic = true;
        preferences.next_check = 200;
        assert!(!preferences.due(199));
        assert!(preferences.due(200));
        preferences.dismissed = Some("1.2.0".into());
        assert!(!preferences.should_notify("1.2.0", false));
        assert!(preferences.should_notify("1.2.0", true));
        assert!(preferences.should_notify("1.2.1", false));
        assert_eq!(next_check(&Ok(None), 100), 100 + DAY);
    }
    fn server(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/releases", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let handle = thread::spawn(move || {
            for response in responses {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut byte = [0];
                while !bytes.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).unwrap();
                    bytes.push(byte[0]);
                }
                recorded
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(bytes).unwrap());
                socket.write_all(response.as_bytes()).unwrap();
            }
        });
        (url, requests, handle)
    }
    #[test]
    fn conditional_cache_and_no_credentials_or_workspace_headers() {
        let body = "[]";
        let (url, requests, handle) = server(vec![
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"test\"\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
            "HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n".into(),
        ]);
        let mut checker = Checker::default();
        assert!(checker.check_from(Channel::Stable, &url).unwrap().is_none());
        assert!(checker.check_from(Channel::Stable, &url).unwrap().is_none());
        handle.join().unwrap();
        let requests = requests.lock().unwrap();
        assert!(!requests[0].to_lowercase().contains("if-none-match"));
        assert!(
            requests[1]
                .to_lowercase()
                .contains("if-none-match: \"test\"")
        );
        for request in requests.iter() {
            let lower = request.to_lowercase();
            assert!(
                !lower.contains("authorization")
                    && !lower.contains("cookie")
                    && !lower.contains("sigma_dock_socket")
            );
            assert!(request.starts_with("GET /releases?per_page=100&page=1 HTTP/1.1"));
        }
    }
    #[test]
    fn offline_and_rate_limit_backoff_do_not_repeat_requests() {
        let (url,requests,handle) = server(vec!["HTTP/1.1 429 Too Many Requests\r\nRetry-After: 600\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()]);
        let mut checker = Checker::default();
        assert_eq!(
            checker
                .check_from(Channel::Stable, &url)
                .unwrap_err()
                .retry_after,
            600
        );
        assert!(
            checker
                .check_from(Channel::Stable, &url)
                .unwrap_err()
                .message
                .contains("paused")
        );
        handle.join().unwrap();
        assert_eq!(requests.lock().unwrap().len(), 1);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let offline = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let mut checker = Checker::default();
        assert!(
            checker
                .check_from(Channel::Stable, &offline)
                .unwrap_err()
                .message
                .contains("connection")
        );
        assert!(checker.not_before > now());
    }
    #[test]
    fn redirects_and_bad_responses_are_errors() {
        for response in [
            "HTTP/1.1 302 Found\r\nLocation: https://example.com/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbad",
        ] {
            let (url, _, handle) = server(vec![response.into()]);
            assert!(Checker::default().fetch(&url).is_err());
            handle.join().unwrap();
        }
    }
}

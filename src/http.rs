use anyhow::{Result, anyhow, bail};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::Mutex;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

pub const EUTILS: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/";

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub api_key: Option<String>,
    pub email: Option<String>,
    pub no_cache: bool,
    pub refresh: bool,
    pub quiet: bool,
    pub cache_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> Config {
        let cache_dir = std::env::var_os("PM_CACHE")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(|p| PathBuf::from(p).join("pm")))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/pm")))
            .unwrap_or_else(|| PathBuf::from(".pm-cache"));
        Config {
            api_key: std::env::var("NCBI_API_KEY").ok().filter(|s| !s.is_empty()),
            email: std::env::var("NCBI_EMAIL").ok().filter(|s| !s.is_empty()),
            cache_dir,
            ..Default::default()
        }
    }
}

struct Limiter {
    next: Mutex<Instant>,
    gap: Duration,
}

impl Limiter {
    fn new(rps: u32) -> Limiter {
        Limiter { next: Mutex::new(Instant::now()), gap: Duration::from_micros(1_000_000 / rps as u64) }
    }

    async fn wait(&self) {
        loop {
            let at = {
                let mut n = self.next.lock().await;
                let now = Instant::now();
                if *n <= now {
                    *n = now + self.gap;
                    return;
                }
                *n
            };
            tokio::time::sleep_until(at.into()).await;
        }
    }
}

pub struct Http {
    client: reqwest::Client,
    ncbi: Limiter,
    other: Limiter,
    pub cfg: Config,
}

pub struct Ctx {
    pub http: Http,
}

impl Ctx {
    pub fn new(cfg: Config) -> Result<Arc<Ctx>> {
        Ok(Arc::new(Ctx { http: Http::new(cfg)? }))
    }

    pub fn cfg(&self) -> &Config {
        &self.http.cfg
    }
}

impl Http {
    pub fn new(cfg: Config) -> Result<Http> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("pm-cli/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_nodelay(true)
            .build()?;
        register_secrets(&cfg);
        let rps = if cfg.api_key.is_some() { 9 } else { 3 };
        Ok(Http { client, ncbi: Limiter::new(rps), other: Limiter::new(8), cfg })
    }

    pub async fn eutils(&self, endpoint: &str, params: &[(&str, &str)]) -> Result<String> {
        let mut p: Vec<(&str, &str)> = params.to_vec();
        p.push(("tool", "pm-cli"));
        if let Some(e) = &self.cfg.email {
            p.push(("email", e));
        }
        if let Some(k) = &self.cfg.api_key {
            p.push(("api_key", k));
        }
        let body = form(&p);
        let url = format!("{EUTILS}{endpoint}");
        self.send(true, move |c| {
            if body.len() > 1500 {
                c.post(&url).header("content-type", "application/x-www-form-urlencoded").body(body.clone())
            } else {
                c.get(format!("{url}?{body}"))
            }
        })
        .await
    }

    pub async fn get(&self, url: &str, params: &[(&str, &str)]) -> Result<String> {
        let full = if params.is_empty() { url.to_string() } else { format!("{url}?{}", form(params)) };
        self.send(false, move |c| c.get(&full)).await
    }

    async fn send<F>(&self, ncbi: bool, build: F) -> Result<String>
    where
        F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    {
        let mut last = anyhow!("request failed");
        for attempt in 0..4u32 {
            if ncbi { self.ncbi.wait().await } else { self.other.wait().await }
            let mut pause = Duration::from_millis(400 << attempt);
            match build(&self.client).send().await {
                Ok(r) => {
                    let st = r.status();
                    if st.is_success() {
                        match r.text().await {
                            Ok(t) if ncbi && is_rate_limited(&t) => {
                                last = anyhow!("ncbi rate limit");
                            }
                            Ok(t) => return Ok(t),
                            Err(e) => last = net_err(e),
                        }
                    } else if st.as_u16() == 429 || st.is_server_error() {
                        let hint = r.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(parse_retry_after);
                        pause = hint.unwrap_or(pause);
                        last = anyhow!("http {st}");
                    } else {
                        let t = r.text().await.unwrap_or_default();
                        bail!("http {st}: {}", redact(&t).chars().take(200).collect::<String>());
                    }
                }
                Err(e) if e.is_timeout() || e.is_connect() || e.is_request() => last = net_err(e),
                Err(e) => return Err(net_err(e)),
            }
            if attempt < 3 {
                tokio::time::sleep(pause).await;
            }
        }
        Err(last)
    }

    fn cache_path(&self, ns: &str, key: &str) -> PathBuf {
        let h = fnv(key);
        self.cfg.cache_dir.join(ns).join(format!("{:02x}", h & 0xff)).join(format!("{h:016x}"))
    }

    pub async fn cache_get(&self, ns: &str, key: &str, ttl: Duration) -> Option<Vec<u8>> {
        if self.cfg.no_cache || self.cfg.refresh {
            return None;
        }
        let p = self.cache_path(ns, key);
        let meta = tokio::fs::metadata(&p).await.ok()?;
        if SystemTime::now().duration_since(meta.modified().ok()?).ok()? > ttl {
            return None;
        }
        let data = tokio::fs::read(&p).await.ok()?;
        let klen = u32::from_le_bytes(data.get(..4)?.try_into().ok()?) as usize;
        if data.get(4..4 + klen)? != key.as_bytes() {
            return None;
        }
        Some(data[4 + klen..].to_vec())
    }

    pub async fn cache_put(&self, ns: &str, key: &str, val: &[u8]) {
        if self.cfg.no_cache {
            return;
        }
        let p = self.cache_path(ns, key);
        if let Some(d) = p.parent() {
            let _ = tokio::fs::create_dir_all(d).await;
        }
        let mut data = Vec::with_capacity(4 + key.len() + val.len());
        data.extend_from_slice(&(key.len() as u32).to_le_bytes());
        data.extend_from_slice(key.as_bytes());
        data.extend_from_slice(val);
        let tmp = p.with_extension(format!("t{}.{}", std::process::id(), TMP_SEQ.fetch_add(1, Ordering::Relaxed)));
        if tokio::fs::write(&tmp, data).await.is_ok() {
            let _ = tokio::fs::rename(&tmp, &p).await;
        }
    }
}

fn parse_retry_after(v: &str) -> Option<Duration> {
    v.trim().parse::<u64>().ok().map(|s| Duration::from_secs(s.min(20)))
}

static SECRETS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

fn register_secrets(cfg: &Config) {
    let v: Vec<String> = [cfg.api_key.as_deref(), cfg.email.as_deref()]
        .into_iter()
        .flatten()
        .flat_map(|s| [s.to_string(), enc(s)])
        .filter(|s| s.len() >= 3)
        .collect();
    let _ = SECRETS.set(v);
}

pub fn redact(s: &str) -> String {
    redact_with(s, SECRETS.get().map_or(&[][..], Vec::as_slice))
}

fn cli_secret_name(w: &str) -> Option<usize> {
    let body = w.trim_start_matches('-');
    let dashes = w.len() - body.len();
    if dashes == 0 || dashes > 2 {
        return None;
    }
    let lower = body.to_ascii_lowercase();
    ["key", "email", "api-key", "api_key", "apikey"]
        .iter()
        .find(|n| lower == **n || lower.strip_prefix(**n).is_some_and(|r| r.starts_with('=')))
        .map(|n| dashes + n.len())
}

fn redact_cli(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut hide_next = false;
    for piece in s.split_inclusive(char::is_whitespace) {
        let tok = piece.trim_end_matches(char::is_whitespace);
        let ws = &piece[tok.len()..];
        let quotes = ['\'', '"'];
        let lead = tok.len() - tok.trim_start_matches(quotes).len();
        let w = &tok[lead..];
        let bare = w.trim_end_matches(quotes);
        if hide_next && !bare.is_empty() && !bare.starts_with('-') && bare != "needs" {
            out.push_str(&tok[..lead]);
            out.push_str("***");
            out.push_str(&w[bare.len()..]);
            out.push_str(ws);
            hide_next = false;
            continue;
        }
        hide_next = false;
        match cli_secret_name(bare) {
            Some(end) if bare[end..].starts_with('=') => {
                out.push_str(&tok[..lead + end + 1]);
                out.push_str("***");
                out.push_str(&w[bare.len()..]);
            }
            Some(_) => {
                hide_next = true;
                out.push_str(tok);
            }
            None => out.push_str(tok),
        }
        out.push_str(ws);
    }
    out
}

fn redact_with(s: &str, secrets: &[String]) -> String {
    let mut out = redact_cli(s);
    for v in secrets {
        if out.contains(v.as_str()) {
            out = out.replace(v.as_str(), "***");
        }
    }
    for key in ["api_key", "api-key", "email"] {
        let mut from = 0;
        while from < out.len()
            && let Some(i) = out.to_ascii_lowercase()[from..].find(key)
        {
            let mut at = from + i + key.len();
            let b = out.as_bytes();
            while at < b.len() && matches!(b[at], b'"' | b'\'' | b' ') {
                at += 1;
            }
            if at >= b.len() || !matches!(b[at], b'=' | b':') {
                from += i + key.len();
                continue;
            }
            at += 1;
            while at < b.len() && matches!(b[at], b'"' | b'\'' | b' ') {
                at += 1;
            }
            let end = out[at..]
                .find(['&', ' ', '"', '\'', ')', '>', '\n', ',', '}'])
                .map_or(out.len(), |e| at + e);
            if end > at {
                out.replace_range(at..end, "***");
                from = at + 3;
            } else {
                from = at;
            }
        }
    }
    out
}

fn is_rate_limited(body: &str) -> bool {
    let t = body.trim_start();
    body.len() < 400
        && t.to_ascii_lowercase().contains("api rate limit exceeded")
        && (t.starts_with("{\"error\"") || t.starts_with("{\"ERROR\"") || t.starts_with("<") || t.starts_with("API rate limit"))
}

fn net_err(e: reqwest::Error) -> anyhow::Error {
    let mut tail = String::new();
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        tail.push_str(": ");
        tail.push_str(&s.to_string());
        src = s.source();
    }
    anyhow!("{}", redact(&format!("{}{tail}", e.without_url())))
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

pub fn form(p: &[(&str, &str)]) -> String {
    p.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_detection_is_strict() {
        assert!(is_rate_limited(r#"{"error":"API rate limit exceeded","api-key":"x","count":"3"}"#));
        assert!(is_rate_limited("<ERROR>API rate limit exceeded</ERROR>"));
        assert!(!is_rate_limited(r#"{"esearchresult":{"count":"5","querytranslation":"\"rate limit\"[Title]"}}"#));
        assert!(!is_rate_limited(""));
    }

    #[test]
    fn scrubs_secrets() {
        let m = "error sending request for url (https://x/y?db=pubmed&api_key=SECRET123&email=a%40b.c&id=1)";
        let s = redact(m);
        assert!(!s.contains("SECRET123") && !s.contains("a%40b.c"), "{s}");
        assert!(s.contains("db=pubmed") && s.contains("id=1"));
        assert_eq!(redact("api_key=abc"), "api_key=***");
        assert_eq!(redact(r#"{"api-key":"SECRET9","error":"bad"}"#), r#"{"api-key":"***","error":"bad"}"#);
        assert_eq!(redact(r#"{"API_KEY": "S3"}"#), r#"{"API_KEY": "***"}"#);
    }

    #[tokio::test]
    async fn cancelled_waiters_leave_no_residue() {
        let l = std::sync::Arc::new(Limiter::new(50));
        l.wait().await;
        let mut hs = Vec::new();
        for _ in 0..25 {
            let l = l.clone();
            hs.push(tokio::spawn(async move {
                let _ = tokio::time::timeout(Duration::from_millis(3), l.wait()).await;
            }));
        }
        for h in hs {
            h.await.unwrap();
        }
        let t = Instant::now();
        l.wait().await;
        assert!(t.elapsed() < Duration::from_millis(60), "{:?}", t.elapsed());
    }

    #[tokio::test]
    async fn waits_respect_rate() {
        let l = std::sync::Arc::new(Limiter::new(50));
        let t = Instant::now();
        let hs: Vec<_> = (0..5)
            .map(|_| {
                let l = l.clone();
                tokio::spawn(async move { l.wait().await })
            })
            .collect();
        for h in hs {
            h.await.unwrap();
        }
        assert!(t.elapsed() >= Duration::from_millis(75), "{:?}", t.elapsed());
    }

    #[test]
    fn cli_credential_options_are_masked_in_any_text() {
        assert_eq!(redact_with("unknown command '--key=SECRET'", &[]), "unknown command '--key=***'");
        assert_eq!(redact_with("bad --key SECRET -n 3", &[]), "bad --key *** -n 3");
        assert_eq!(redact_with("-key=SECRET x", &[]), "-key=*** x");
        assert_eq!(redact_with("--email=a@b.c", &[]), "--email=***");
        assert_eq!(redact_with("--api-key SECRET", &[]), "--api-key ***");
        assert_eq!(redact_with("--key\tSECRET\nnext", &[]), "--key\t***\nnext");
        assert_eq!(redact_with("--key needs a value", &[]), "--key needs a value");
        assert_eq!(redact_with("--key --fmt ids", &[]), "--key --fmt ids");
        assert_eq!(redact_with("--keyword abc", &[]), "--keyword abc");
        assert_eq!(redact_with("line one\nline two\n", &[]), "line one\nline two\n");
    }

    #[test]
    fn empty_credential_values_do_not_panic() {
        for s in ["api_key=", "email=", "bad id 'email='", "{\"api-key\":\"\"}", "email"] {
            assert_eq!(redact_with(s, &[]), s);
        }
        assert_eq!(redact_with("api_key=abc&x=1", &[]), "api_key=***&x=1");
    }

    #[test]
    fn redacts_configured_values() {
        let sec = vec!["TOPSECRET".to_string(), "me%40x.org".to_string(), "me@x.org".to_string()];
        let s = redact_with("bad key TOPSECRET for me@x.org / me%40x.org", &sec);
        assert_eq!(s, "bad key *** for *** / ***");
        assert_eq!(redact_with("nothing here", &sec), "nothing here");
    }

    #[test]
    fn retry_after_seconds_capped() {
        assert_eq!(parse_retry_after("3"), Some(Duration::from_secs(3)));
        assert_eq!(parse_retry_after(" 120 "), Some(Duration::from_secs(20)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
    }

    #[tokio::test]
    async fn connect_error_hides_key() {
        let cfg = Config { api_key: Some("review-sentinel-key".into()), no_cache: true, ..Default::default() };
        let h = Http::new(cfg).unwrap();
        let e = h.get("http://127.0.0.1:1/x", &[("api_key", "review-sentinel-key")]).await.unwrap_err();
        assert!(!format!("{e:#}").contains("review-sentinel-key"), "{e:#}");
    }
}

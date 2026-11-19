use crate::model::Track;
use anyhow::{ensure, Context};
use ncm_api_rs::{ApiClient, ApiResponse, Query};
use serde_json::Value;
use std::collections::BTreeMap;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Retries transport-level failures only: flaky DNS lookups, TLS handshakes and dropped
/// connections are common, while an API answer the user must see is returned immediately.
macro_rules! send {
    ($label:expr, $call:expr) => {{
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let outcome: anyhow::Result<ApiResponse> = async { anyhow::Ok($call.await??) }.await;
            match outcome {
                Ok(response) => break anyhow::Ok(response),
                Err(error) if attempt < 3 && retryable(&error) => {
                    tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
                }
                Err(error) => break Err(error).context(format!("{}失败", $label)),
            }
        }
    }};
}

fn retryable(error: &anyhow::Error) -> bool {
    if error.chain().any(|cause| cause.is::<std::io::Error>()) {
        return true;
    }
    let text = format!("{error:#}").to_ascii_lowercase();
    [
        "error sending request",
        "timed out",
        "elapsed",
        "connect",
        "dns",
        "tls",
        "ssl",
        "reset",
        "broken pipe",
        "unexpected eof",
        "os error",
        "network",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

pub fn merge_cookies(current: &mut BTreeMap<String, String>, set_cookies: &[String]) {
    for raw in set_cookies {
        if let Some((key, value)) = raw.split(';').next().unwrap_or("").split_once('=') {
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            if value.is_empty()
                || raw
                    .split(';')
                    .any(|a| a.trim().eq_ignore_ascii_case("Max-Age=0"))
            {
                current.remove(key);
            } else {
                current.insert(key.into(), value.trim().into());
            }
        }
    }
}
pub fn playlist_ids(body: &Value) -> anyhow::Result<Vec<u64>> {
    let playlist = &body["playlist"];
    let ids = playlist["trackIds"]
        .as_array()
        .context("歌单未返回完整曲目 ID，请重试")?
        .iter()
        .filter_map(|s| s["id"].as_u64())
        .collect::<Vec<_>>();
    let count = playlist["trackCount"].as_u64().unwrap_or(ids.len() as u64);
    ensure!(
        ids.len() as u64 >= count,
        "歌单只返回了 {}/{} 个曲目 ID，已停止加载以避免遗漏",
        ids.len(),
        count
    );
    Ok(ids)
}
pub fn order_tracks(ids: &[u64], tracks: Vec<Track>) -> Vec<Track> {
    let mut tracks = tracks
        .into_iter()
        .filter_map(|s| s.id.map(|id| (id, s)))
        .collect::<BTreeMap<_, _>>();
    ids.iter().filter_map(|id| tracks.remove(id)).collect()
}
pub fn playback_url(body: &Value) -> anyhow::Result<(String, bool)> {
    let item = &body["data"][0];
    let value = item["url"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("此歌曲暂无可播放音源，或超出账号权益")?;
    let url = url::Url::parse(value)?;
    ensure!(
        ["http", "https"].contains(&url.scheme()),
        "平台返回了非网络播放地址"
    );
    // Keep the scheme the platform handed out: MediaPlayer stalls at 0 ms on the
    // https form of an http media URL, while the address itself plays fine.
    Ok((url.to_string(), !item["freeTrialInfo"].is_null()))
}

#[derive(Debug, Clone)]
pub struct Playlist {
    pub id: u64,
    pub name: String,
    pub count: u64,
}

pub struct CloudClient {
    client: ApiClient,
    cookies: BTreeMap<String, String>,
    session: PathBuf,
    logged_in: bool,
}
impl CloudClient {
    pub fn new(data: &Path, device_id: &str) -> Self {
        let session = data.join("session.dpapi");
        let cookies = std::fs::read(&session)
            .ok()
            .and_then(|bytes| crate::credentials::unprotect(&bytes).ok())
            .and_then(|bytes| serde_json::from_slice::<BTreeMap<String, String>>(&bytes).ok())
            .unwrap_or_default();
        Self::with_cookies(session, device_id, cookies)
    }
    pub fn sign_in(data: &Path, device_id: &str) -> Self {
        Self::with_cookies(data.join("session.dpapi"), device_id, BTreeMap::new())
    }
    fn with_cookies(session: PathBuf, device_id: &str, cookies: BTreeMap<String, String>) -> Self {
        let cookie = cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        let mut client = ncm_api_rs::create_client(Some(cookie));
        client.set_device_id(device_id.into());
        Self {
            client,
            cookies,
            session,
            logged_in: false,
        }
    }
    pub fn has_session(&self) -> bool {
        self.cookies.contains_key("MUSIC_U")
    }
    fn accept(&mut self, response: ApiResponse) -> anyhow::Result<Value> {
        let before = self.cookies.clone();
        merge_cookies(&mut self.cookies, &response.cookie);
        if before != self.cookies {
            self.client.set_cookie(
                self.cookies
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            );
            if self.logged_in {
                self.save_session()?;
            }
        }
        Ok(response.body)
    }
    fn save_session(&self) -> anyhow::Result<()> {
        crate::persistence::save_bytes(
            &self.session,
            &crate::credentials::protect(&serde_json::to_vec(&self.cookies)?)?,
        )
    }
    pub fn commit_session(&self) -> anyhow::Result<()> {
        self.save_session()
    }
    pub fn logout(&mut self) -> anyhow::Result<()> {
        // Revoke only this local session; do not send account-wide logout to the platform.
        self.cookies.clear();
        self.client.set_cookie(String::new());
        self.logged_in = false;
        crate::persistence::save_bytes(&self.session, &crate::credentials::protect(b"{}")?)
    }
    pub async fn account(&mut self) -> anyhow::Result<Option<(u64, String)>> {
        let response = send!(
            "读取账号",
            tokio::time::timeout(
                Duration::from_secs(18),
                self.client.user_account(&Query::new()),
            )
        )?;
        let body = self.accept(response)?;
        let profile = body.get("profile").or_else(|| body["data"].get("profile"));
        if let Some(profile) = profile {
            if let Some(id) = profile["userId"].as_u64() {
                self.logged_in = true;
                return Ok(Some((
                    id,
                    profile["nickname"].as_str().unwrap_or("网易云用户").into(),
                )));
            }
        }
        self.logged_in = false;
        Ok(None)
    }
    pub async fn qr_key(&mut self) -> anyhow::Result<String> {
        let response = send!(
            "获取登录二维码",
            tokio::time::timeout(
                Duration::from_secs(18),
                self.client.login_qr_key(&Query::new()),
            )
        )?;
        let body = self.accept(response)?;
        body["unikey"]
            .as_str()
            .or_else(|| body["data"]["unikey"].as_str())
            .map(str::to_string)
            .context("平台未返回登录二维码")
    }
    pub async fn qr_check(&mut self, key: &str) -> anyhow::Result<u64> {
        let response = send!(
            "检查扫码状态",
            tokio::time::timeout(
                Duration::from_secs(12),
                self.client.login_qr_check(&Query::new().param("key", key)),
            )
        )?;
        let body = self.accept(response)?;
        Ok(body["code"].as_u64().unwrap_or(800))
    }
    pub async fn search(
        &mut self,
        keywords: &str,
        offset: usize,
    ) -> anyhow::Result<(Vec<Track>, usize)> {
        let query = Query::new()
            .param("keywords", keywords)
            .param("type", "1")
            .param("limit", "100")
            .param("offset", &offset.to_string());
        let response = send!(
            "搜索歌曲",
            tokio::time::timeout(Duration::from_secs(18), self.client.cloudsearch(&query))
        )?;
        let body = self.accept(response)?;
        ensure!(
            body["result"]["songs"].is_array() || body["result"]["songCount"].as_u64() == Some(0),
            "搜索响应异常"
        );
        Ok((
            crate::model::parse_tracks(&body["result"]["songs"]),
            body["result"]["songCount"].as_u64().unwrap_or(0) as usize,
        ))
    }
    pub async fn playlists(&mut self, user: u64) -> anyhow::Result<Vec<Playlist>> {
        let mut all = Vec::new();
        let mut offset = 0;
        loop {
            let query = Query::new()
                .param("uid", &user.to_string())
                .param("limit", "200")
                .param("offset", &offset.to_string());
            let response = send!(
                "读取歌单",
                tokio::time::timeout(Duration::from_secs(18), self.client.user_playlist(&query))
            )?;
            let body = self.accept(response)?;
            let list = body["playlist"].as_array().context("平台未返回歌单列表")?;
            all.extend(list.iter().filter_map(|p| {
                Some(Playlist {
                    id: p["id"].as_u64()?,
                    name: p["name"].as_str().unwrap_or("歌单").into(),
                    count: p["trackCount"].as_u64().unwrap_or(0),
                })
            }));
            if !body["more"].as_bool().unwrap_or(false) || list.is_empty() {
                break;
            }
            offset += list.len();
            ensure!(offset < 100000, "歌单数量超过安全上限");
        }
        Ok(all)
    }
    pub async fn playlist(&mut self, id: u64) -> anyhow::Result<Vec<Track>> {
        let response = send!(
            "读取歌单详情",
            tokio::time::timeout(
                Duration::from_secs(18),
                self.client
                    .playlist_detail(&Query::new().param("id", &id.to_string()).param("s", "0")),
            )
        )?;
        let body = self.accept(response)?;
        let ids = playlist_ids(&body)?;
        let mut tracks = Vec::with_capacity(ids.len());
        // The embedded tracks field may contain only the first 10/1000 tracks.
        for chunk in ids.chunks(200) {
            let query = Query::new().param(
                "ids",
                &chunk
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            );
            let response = send!(
                "读取歌曲信息",
                tokio::time::timeout(Duration::from_secs(18), self.client.song_detail(&query))
            )?;
            let body = self.accept(response)?;
            tracks.extend(crate::model::parse_tracks(&body["songs"]));
        }
        let ordered = order_tracks(&ids, tracks);
        ensure!(
            ordered.len() == ids.len(),
            "歌单详情不完整：取得 {}/{} 首，请重试",
            ordered.len(),
            ids.len()
        );
        Ok(ordered)
    }
    /// The EAPI player endpoint answers `code = -460` ("检测到您的网络环境存在风险")
    /// from many home networks, so the plain `/api/song/enhance/player/url` endpoint is
    /// asked first: it keeps answering, and `br = 999000` still asks for the best
    /// bitrate the account may stream. The EAPI endpoint stays as a second chance for
    /// songs the plain endpoint cannot resolve.
    pub async fn song_url(&mut self, id: u64) -> anyhow::Result<(String, bool)> {
        let plain = Query::new()
            .param("id", &id.to_string())
            .param("br", "999000");
        let plain = send!(
            "获取播放地址",
            tokio::time::timeout(Duration::from_secs(18), self.client.song_url(&plain))
        );
        match plain {
            Ok(response) => {
                let body = self.accept(response)?;
                match playback_url(&body) {
                    Ok(play) => Ok(play),
                    // No audio is an answer, not a failure: try the other endpoint but
                    // keep "no source for this account" when that one fails too.
                    Err(no_source) => self.song_url_modern(id).await.or(Err(no_source)),
                }
            }
            Err(plain) => match self.song_url_modern(id).await {
                Ok(play) => Ok(play),
                Err(modern) => Err(anyhow::anyhow!("{plain:#}\n新版接口同样失败：{modern:#}")),
            },
        }
    }
    async fn song_url_modern(&mut self, id: u64) -> anyhow::Result<(String, bool)> {
        let query = Query::new()
            .param("id", &id.to_string())
            .param("level", "exhigh");
        let response = send!(
            "获取播放地址",
            tokio::time::timeout(Duration::from_secs(18), self.client.song_url_v1(&query))
        )?;
        let body = self.accept(response)?;
        playback_url(&body)
    }
}

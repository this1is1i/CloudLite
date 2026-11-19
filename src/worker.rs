//! One sleeping worker owns the API runtime. No local HTTP server or background polling
//! exists outside an explicitly opened QR sign-in session.
use cloudlite::{
    cloud::{CloudClient, Playlist},
    model::Track,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    time::{Duration, Instant},
};

pub enum Command {
    PickFolder(usize),
    Restore,
    Login(u64),
    CancelLogin(u64),
    Logout,
    Playlists(u64),
    Search {
        epoch: u64,
        keywords: String,
        offset: usize,
    },
    Playlist {
        epoch: u64,
        id: u64,
    },
    Scan(Vec<PathBuf>),
    Resolve {
        epoch: u64,
        track: Track,
        local: Option<PathBuf>,
    },
}
pub enum Event {
    FolderPicked(Result<Option<PathBuf>, String>),
    Account {
        epoch: Option<u64>,
        account: Option<(u64, String)>,
    },
    Qr {
        epoch: u64,
        url: String,
    },
    QrStatus {
        epoch: u64,
        status: String,
        closed: bool,
    },
    Playlists(Vec<Playlist>),
    Tracks {
        epoch: u64,
        tracks: Vec<Track>,
        offset: usize,
        total: usize,
    },
    Library {
        tracks: Vec<Track>,
        warnings: Vec<String>,
    },
    ScanProgress {
        done: usize,
        total: usize,
    },
    Ready {
        epoch: u64,
        track: Track,
        source: windows::Media::Core::MediaSource,
        temp: Option<tempfile::NamedTempFile>,
        local: bool,
        preview: bool,
    },
    Error(String),
    PlaybackError {
        epoch: u64,
        message: String,
    },
    ViewError {
        epoch: u64,
        message: String,
    },
}

pub fn spawn(
    data: PathBuf,
    device_id: String,
    play_epoch: Arc<AtomicU64>,
    login_gate: Arc<AtomicU64>,
    wake: impl Fn() + Send + Sync + 'static,
) -> (Sender<Command>, Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let (events, event_rx) = mpsc::channel();
    let wake = Arc::new(wake);
    std::thread::Builder::new()
        .name("cloudlite-worker".into())
        .spawn(move || {
            unsafe {
                let _ = windows::Win32::System::WinRT::RoInitialize(
                    windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
                );
            }
            let emit = |event| {
                if events.send(event).is_ok() {
                    wake();
                }
            };
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    emit(Event::Error(e.to_string()));
                    return;
                }
            };
            let mut cloud = CloudClient::new(&data, &device_id);
            let mut qr: Option<(u64, String, Instant, Instant, CloudClient)> = None;
            loop {
                let timeout = qr
                    .as_ref()
                    .map(|q| q.3.saturating_duration_since(Instant::now()))
                    .unwrap_or(Duration::from_secs(86400));
                match rx.recv_timeout(timeout) {
                    Ok(command) => {
                        let result: anyhow::Result<()> = runtime.block_on(async {
                            match command {
                                Command::PickFolder(owner) => {
                                    let sender = events.clone();
                                    let notify = wake.clone();
                                    // Shell extensions are loaded only in this short-lived process.
                                    // Keeping the wait off the API worker also preserves background playback.
                                    std::thread::Builder::new()
                                        .name("cloudlite-picker".into())
                                        .spawn(move || {
                                            use std::os::windows::process::CommandExt;
                                            let picked = (|| -> anyhow::Result<Option<PathBuf>> {
                                                let output = std::process::Command::new(
                                                    std::env::current_exe()?,
                                                )
                                                .args([
                                                    "--pick-folder",
                                                    "--owner",
                                                    &owner.to_string(),
                                                ])
                                                .creation_flags(0x08000000)
                                                .output()?;
                                                anyhow::ensure!(
                                                    output.status.success(),
                                                    "目录选择器异常退出"
                                                );
                                                cloudlite::picker::decode_result(&output.stdout)
                                            })(
                                            )
                                            .map_err(|e| format!("{e:#}"));
                                            if sender.send(Event::FolderPicked(picked)).is_ok() {
                                                notify();
                                            }
                                        })?;
                                }
                                Command::Restore => {
                                    if cloud.has_session() {
                                        let account = cloud.account().await?;
                                        if account.is_some() {
                                            cloud.commit_session()?;
                                        }
                                        emit(Event::Account {
                                            epoch: None,
                                            account,
                                        });
                                    }
                                }
                                Command::Login(epoch) => {
                                    qr = None;
                                    if login_gate.load(Ordering::SeqCst) != epoch {
                                        return Ok(());
                                    }
                                    let mut candidate = CloudClient::sign_in(&data, &device_id);
                                    let key = candidate.qr_key().await?;
                                    if login_gate.load(Ordering::SeqCst) == epoch {
                                        emit(Event::Qr {
                                            epoch,
                                            url: format!(
                                                "https://music.163.com/login?codekey={key}"
                                            ),
                                        });
                                        qr = Some((
                                            epoch,
                                            key,
                                            Instant::now(),
                                            Instant::now() + Duration::from_secs(2),
                                            candidate,
                                        ));
                                    }
                                }
                                Command::CancelLogin(epoch) => {
                                    if qr.as_ref().is_some_and(|q| q.0 == epoch) {
                                        qr = None;
                                    }
                                }
                                Command::Logout => {
                                    qr = None;
                                    cloud.logout()?;
                                    emit(Event::Account {
                                        epoch: None,
                                        account: None,
                                    });
                                }
                                Command::Playlists(user) => {
                                    emit(Event::Playlists(cloud.playlists(user).await?));
                                }
                                Command::Search {
                                    epoch,
                                    keywords,
                                    offset,
                                } => match cloud.search(&keywords, offset).await {
                                    Ok((tracks, total)) => emit(Event::Tracks {
                                        epoch,
                                        tracks,
                                        offset,
                                        total,
                                    }),
                                    Err(e) => emit(Event::ViewError {
                                        epoch,
                                        message: format!("{e:#}"),
                                    }),
                                },
                                Command::Playlist { epoch, id } => match cloud.playlist(id).await {
                                    Ok(tracks) => {
                                        let total = tracks.len();
                                        emit(Event::Tracks {
                                            epoch,
                                            tracks,
                                            offset: 0,
                                            total,
                                        });
                                    }
                                    Err(e) => emit(Event::ViewError {
                                        epoch,
                                        message: format!("{e:#}"),
                                    }),
                                },
                                Command::Scan(roots) => {
                                    let sender = events.clone();
                                    let notify = wake.clone();
                                    let library = data.join("library.json");
                                    // Scanning runs off the API worker so QR polling and playback
                                    // control stay responsive while tags are read.
                                    std::thread::Builder::new()
                                        .name("cloudlite-scan".into())
                                        .spawn(move || {
                                            let mut reported = Instant::now()
                                                .checked_sub(Duration::from_secs(1))
                                                .unwrap_or_else(Instant::now);
                                            let (tracks, warnings) = cloudlite::library::scan_with(
                                                &roots,
                                                |done, total| {
                                                    let finished = total > 0 && done >= total;
                                                    if !finished
                                                        && reported.elapsed()
                                                            < Duration::from_millis(120)
                                                    {
                                                        return;
                                                    }
                                                    reported = Instant::now();
                                                    if sender
                                                        .send(Event::ScanProgress { done, total })
                                                        .is_ok()
                                                    {
                                                        notify();
                                                    }
                                                },
                                            );
                                            let saved = serde_json::to_value(&tracks)
                                                .map_err(anyhow::Error::from)
                                                .and_then(|value| {
                                                    cloudlite::persistence::save_json(
                                                        &library, &value,
                                                    )
                                                });
                                            let event = match saved {
                                                Ok(()) => Event::Library { tracks, warnings },
                                                Err(error) => {
                                                    Event::Error(format!("保存曲库失败：{error:#}"))
                                                }
                                            };
                                            if sender.send(event).is_ok() {
                                                notify();
                                            }
                                        })?;
                                }
                                Command::Resolve {
                                    epoch,
                                    track,
                                    local,
                                } => {
                                    if play_epoch.load(Ordering::SeqCst) != epoch {
                                        return Ok(());
                                    }
                                    let result: anyhow::Result<Event> = async {
                                        if let Some(path) = local.filter(|p| p.is_file()) {
                                            let temp = if path
                                                .extension()
                                                .is_some_and(|e| e.eq_ignore_ascii_case("ncm"))
                                            {
                                                Some(cloudlite::ncm::prepare(
                                                    &path,
                                                    &data.join("cache"),
                                                )?)
                                            } else {
                                                None
                                            };
                                            let source =
                                                temp.as_ref().map(|t| t.path()).unwrap_or(&path);
                                            let uri = url::Url::from_file_path(source)
                                                .map_err(|_| {
                                                    anyhow::anyhow!("本地路径无法转换为播放地址")
                                                })?
                                                .to_string();
                                            let source = cloudlite::media::source_for_uri(&uri)?;
                                            Ok(Event::Ready {
                                                epoch,
                                                track,
                                                source,
                                                temp,
                                                local: true,
                                                preview: false,
                                            })
                                        } else {
                                            let id = track.id.ok_or_else(|| {
                                                anyhow::anyhow!(
                                                    "本地文件已移动或不存在，请重新扫描"
                                                )
                                            })?;
                                            let (uri, preview) = cloud.song_url(id).await?;
                                            let source = cloudlite::media::source_for_uri(&uri)?;
                                            Ok(Event::Ready {
                                                epoch,
                                                track,
                                                source,
                                                temp: None,
                                                local: false,
                                                preview,
                                            })
                                        }
                                    }
                                    .await;
                                    if play_epoch.load(Ordering::SeqCst) == epoch {
                                        match result {
                                            Ok(event) => emit(event),
                                            Err(e) => emit(Event::PlaybackError {
                                                epoch,
                                                message: format!("{e:#}"),
                                            }),
                                        }
                                    }
                                }
                            }
                            Ok(())
                        });
                        if let Err(error) = result {
                            emit(Event::Error(format!("{error:#}")));
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if qr.as_ref().is_some_and(|q| Instant::now() >= q.3) {
                    let (epoch, key, started, _, mut candidate) = qr.take().unwrap();
                    if login_gate.load(Ordering::SeqCst) != epoch {
                        continue;
                    }
                    if started.elapsed() > Duration::from_secs(180) {
                        emit(Event::QrStatus {
                            epoch,
                            status: "二维码已过期。关闭后点击扫码登录可刷新。".into(),
                            closed: false,
                        });
                        continue;
                    }
                    let checked = runtime.block_on(candidate.qr_check(&key));
                    if login_gate.load(Ordering::SeqCst) != epoch {
                        continue;
                    }
                    match checked {
                        Ok(803) => {
                            let account = runtime.block_on(candidate.account());
                            if login_gate.load(Ordering::SeqCst) != epoch {
                                continue;
                            }
                            match account {
                                Ok(Some(account)) => match candidate.commit_session() {
                                    Ok(()) => {
                                        cloud = candidate;
                                        emit(Event::QrStatus {
                                            epoch,
                                            status: "登录成功".into(),
                                            closed: true,
                                        });
                                        emit(Event::Account {
                                            epoch: Some(epoch),
                                            account: Some(account),
                                        });
                                    }
                                    Err(e) => {
                                        emit(Event::Error(format!("登录凭据保存失败：{e:#}")))
                                    }
                                },
                                Ok(None) => emit(Event::Error(
                                    "扫码已确认，但平台尚未返回账号信息，请重试。".into(),
                                )),
                                Err(e) => emit(Event::Error(format!("账号确认失败：{e:#}"))),
                            }
                        }
                        Ok(800) => emit(Event::QrStatus {
                            epoch,
                            status: "二维码已过期。关闭后重新扫码登录。".into(),
                            closed: false,
                        }),
                        Ok(code) => {
                            emit(Event::QrStatus {
                                epoch,
                                status: if code == 802 {
                                    "已扫码，请在网易云 App 上确认。"
                                } else {
                                    "请用网易云音乐 App 扫码登录。"
                                }
                                .into(),
                                closed: false,
                            });
                            qr = Some((
                                epoch,
                                key,
                                started,
                                Instant::now() + Duration::from_secs(2),
                                candidate,
                            ));
                        }
                        Err(e) => emit(Event::QrStatus {
                            epoch,
                            status: format!("登录检查失败：{e}。关闭后可重试。"),
                            closed: false,
                        }),
                    }
                }
            }
        })
        .expect("create background worker");
    (tx, event_rx)
}

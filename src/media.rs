use windows::{
    core::HSTRING,
    Foundation::{TimeSpan, Uri},
    Media::{
        Core::MediaSource,
        Playback::{MediaPlaybackState, MediaPlayer},
    },
};

pub struct Player {
    pub native: MediaPlayer,
}
impl Player {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            native: MediaPlayer::new()?,
        })
    }
    pub fn play_uri(&self, uri: &str) -> anyhow::Result<()> {
        self.play_source(&source_for_uri(uri)?)
    }
    pub fn play_source(&self, source: &MediaSource) -> anyhow::Result<()> {
        self.load_source(source, true)
    }
    pub fn load_source(&self, source: &MediaSource, play: bool) -> anyhow::Result<()> {
        self.native.SetAutoPlay(false)?;
        self.native.SetSource(source)?;
        if play {
            self.native.Play()?;
        }
        Ok(())
    }
    pub fn pause(&self) -> anyhow::Result<()> {
        self.native.Pause()?;
        Ok(())
    }
    pub fn resume(&self) -> anyhow::Result<()> {
        self.native.Play()?;
        Ok(())
    }
    pub fn seek_ms(&self, ms: u64) -> anyhow::Result<()> {
        self.native.PlaybackSession()?.SetPosition(TimeSpan {
            Duration: ms.min(self.duration_ms()) as i64 * 10_000,
        })?;
        Ok(())
    }
    pub fn position_ms(&self) -> u64 {
        self.native
            .PlaybackSession()
            .and_then(|s| s.Position())
            .map(|p| (p.Duration.max(0) / 10_000) as u64)
            .unwrap_or(0)
    }
    pub fn duration_ms(&self) -> u64 {
        self.native
            .PlaybackSession()
            .and_then(|s| s.NaturalDuration())
            .map(|p| (p.Duration.max(0) / 10_000) as u64)
            .unwrap_or(0)
    }
    pub fn set_volume(&self, volume: u32) -> anyhow::Result<()> {
        self.native.SetVolume(volume.min(100) as f64 / 100.)?;
        Ok(())
    }
    pub fn is_playing(&self) -> bool {
        self.native
            .PlaybackSession()
            .and_then(|s| s.PlaybackState())
            .is_ok_and(|s| s == MediaPlaybackState::Playing)
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.native.Close();
    }
}

pub fn source_for_uri(uri: &str) -> anyhow::Result<MediaSource> {
    let parsed = url::Url::parse(uri)?;
    if parsed.scheme() == "file" {
        let path = parsed
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("无效的本地文件地址"))?;
        let file = windows::Storage::StorageFile::GetFileFromPathAsync(&HSTRING::from(
            path.to_string_lossy().as_ref(),
        ))?
        .get()?;
        Ok(MediaSource::CreateFromStorageFile(&file)?)
    } else {
        anyhow::ensure!(
            ["http", "https"].contains(&parsed.scheme()),
            "不支持的播放地址类型"
        );
        Ok(MediaSource::CreateFromUri(&Uri::CreateUri(
            &HSTRING::from(uri),
        )?)?)
    }
}

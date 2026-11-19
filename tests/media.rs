use cloudlite::media::Player;
use std::{
    io::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

#[test]
fn system_audio_engine_plays_pauses_and_seeks_a_local_unicode_file() {
    unsafe {
        RoInitialize(RO_INIT_MULTITHREADED).unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("静音验证.wav");
    let mut f = std::fs::File::create(&path).unwrap();
    let samples = 44100u32 * 4;
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + samples * 2).to_le_bytes()).unwrap();
    f.write_all(b"WAVEfmt ").unwrap();
    f.write_all(&16_u32.to_le_bytes()).unwrap();
    f.write_all(&1_u16.to_le_bytes()).unwrap();
    f.write_all(&1_u16.to_le_bytes()).unwrap();
    f.write_all(&44100_u32.to_le_bytes()).unwrap();
    f.write_all(&88200_u32.to_le_bytes()).unwrap();
    f.write_all(&2_u16.to_le_bytes()).unwrap();
    f.write_all(&16_u16.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&(samples * 2).to_le_bytes()).unwrap();
    f.write_all(&vec![0; samples as usize * 2]).unwrap();
    drop(f);
    let player = Player::new().unwrap();
    player.set_volume(0).unwrap();
    let failure = Arc::new(Mutex::new(String::new()));
    let fail = failure.clone();
    player
        .native
        .MediaFailed(&windows::Foundation::TypedEventHandler::new(
            move |_,
                  args: windows::core::Ref<
                '_,
                windows::Media::Playback::MediaPlayerFailedEventArgs,
            >| {
                if let Some(args) = args.as_ref() {
                    *fail.lock().unwrap() =
                        format!("{:?}: {}", args.ExtendedErrorCode(), args.ErrorMessage()?);
                }
                Ok(())
            },
        ))
        .unwrap();
    player
        .play_uri(
            url::Url::from_file_path(path.canonicalize().unwrap())
                .unwrap()
                .as_str(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while player.position_ms() < 120 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(
        player.position_ms() >= 120,
        "native playback clock did not advance; state={:?}, failure={}",
        player
            .native
            .PlaybackSession()
            .and_then(|s| s.PlaybackState()),
        failure.lock().unwrap()
    );
    assert!((3900..=4100).contains(&player.duration_ms()));
    player.pause().unwrap();
    std::thread::sleep(Duration::from_millis(80));
    let paused = player.position_ms();
    std::thread::sleep(Duration::from_millis(150));
    assert!(player.position_ms().abs_diff(paused) < 50);
    player.seek_ms(2000).unwrap();
    assert!(player.position_ms().abs_diff(2000) < 100);
    player.resume().unwrap();
    std::thread::sleep(Duration::from_millis(180));
    assert!(player.position_ms() > 2100);
}

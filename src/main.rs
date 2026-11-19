#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod gui;
mod theme;
mod worker;

use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        let message = format!("云简启动失败：{error:#}");
        eprintln!("{message}");
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
                None,
                &windows::core::HSTRING::from(message),
                windows::core::w!("云简"),
                windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
    }
}
fn run() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let option = |flag: &str| {
        args.iter()
            .position(|s| s == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    if args.iter().any(|s| s == "--pick-folder") {
        let owner = option("--owner")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let result = match gui::pick_folder_helper(owner) {
            Ok(path) => serde_json::json!({"path":path}),
            Err(e) => serde_json::json!({"error":format!("{e:#}")}),
        };
        println!("{}", serde_json::to_string(&result)?);
        return Ok(());
    }
    let data = option("--data-dir")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_exe()?.parent().unwrap().join("data"));
    std::fs::create_dir_all(&data)?;
    if args.iter().any(|s| s == "--probe-api") {
        unsafe {
            windows::Win32::System::WinRT::RoInitialize(
                windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
            )?;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let report=runtime.block_on(async {
            let mut client=cloudlite::cloud::CloudClient::new(&data,"cloudlite-readonly-connectivity-probe");
            let (songs,total)=client.search("晴天",0).await?;
            anyhow::ensure!(!songs.is_empty(),"搜索没有返回歌曲");
            let qr=client.qr_key().await?;
            anyhow::ensure!(!qr.is_empty(),"二维码密钥为空");
            let (free_candidates,_)=client.search("纯音乐",0).await?;
            let mut playable=false;let mut stream_played=false;let mut checks=Vec::new();
            for song in free_candidates.iter().take(3) {
                let result=client.song_url(song.id.unwrap()).await;
                checks.push(serde_json::json!({"id":song.id,"received_url":result.is_ok(),"error":result.as_ref().err().map(|e|format!("{e:#}"))}));
                if let Ok((uri,_))=result{
                    playable=true;
                    let player=cloudlite::media::Player::new()?;player.set_volume(0)?;player.play_uri(&uri)?;
                    let deadline=std::time::Instant::now()+std::time::Duration::from_secs(20);
                    while player.position_ms()<150 && std::time::Instant::now()<deadline{std::thread::sleep(std::time::Duration::from_millis(50));}
                    stream_played=player.position_ms()>=150;break;
                }
            }
            Ok::<_,anyhow::Error>(serde_json::json!({"search_count":songs.len(),"total":total,"qr_key_received":true,"public_playback_url_received":playable,"stream_playback_clock_advanced":stream_played,"playback_checks":checks,"authenticated":false}))
        });
        let report = match report {
            Ok(v) => v,
            Err(e) => serde_json::json!({"error":format!("{e:#}")}),
        };
        let output = option("--report")
            .map(PathBuf::from)
            .unwrap_or(data.join("api-probe.json"));
        cloudlite::persistence::save_json(&output, &report)?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    gui::run(
        data,
        option("--library").map(PathBuf::from),
        option("--play").map(PathBuf::from),
    )
}

use crate::model::Track;
use lofty::{
    config::ParseOptions,
    file::{AudioFile, TaggedFileExt},
    probe::Probe,
    tag::Accessor,
};
use std::{
    collections::HashSet,
    fs::File,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

const EXTENSIONS: [&str; 10] = [
    "mp3", "flac", "wav", "m4a", "aac", "ogg", "opus", "wma", "ape", "ncm",
];
const MAX_THREADS: usize = 8;

pub fn scan(roots: &[PathBuf]) -> (Vec<Track>, Vec<String>) {
    scan_with(roots, |_, _| {})
}

/// Reads every track below the roots. The progress callback runs on the calling thread and
/// receives (found, 0) while directories are walked and (done, total) while tags are read,
/// so a progress bar can move before the library size is known.
pub fn scan_with(
    roots: &[PathBuf],
    mut progress: impl FnMut(usize, usize),
) -> (Vec<Track>, Vec<String>) {
    let (paths, mut warnings) = discover(roots, &mut progress);
    let total = paths.len();
    if total == 0 {
        progress(0, 0);
        return (Vec::new(), warnings);
    }
    let threads = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .clamp(1, MAX_THREADS)
        .min(total);
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let mut tracks = Vec::with_capacity(total);
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            handles.push(scope.spawn(|| {
                let mut local = Vec::new();
                let mut failed = Vec::new();
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= total {
                        break;
                    }
                    match read_track(&paths[index]) {
                        Ok(track) => local.push((index, track)),
                        Err(error) => {
                            if failed.len() < 20 {
                                failed.push(error);
                            }
                        }
                    }
                    done.fetch_add(1, Ordering::Relaxed);
                }
                (local, failed)
            }));
        }
        loop {
            let finished = done.load(Ordering::Relaxed);
            if finished >= total {
                break;
            }
            progress(finished, total);
            std::thread::sleep(Duration::from_millis(100));
        }
        for handle in handles {
            match handle.join() {
                Ok((local, failed)) => {
                    tracks.extend(local);
                    warnings.extend(failed);
                }
                Err(_) => warnings.push("读取音乐标签的线程异常退出".into()),
            }
        }
    });
    tracks.sort_by(|a, b| {
        a.1.title
            .cmp(&b.1.title)
            .then_with(|| a.1.artist.cmp(&b.1.artist))
    });
    warnings.truncate(20);
    progress(total, total);
    (
        tracks.into_iter().map(|(_, track)| track).collect(),
        warnings,
    )
}

fn discover(
    roots: &[PathBuf],
    progress: &mut impl FnMut(usize, usize),
) -> (Vec<PathBuf>, Vec<String>) {
    let mut paths = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        for entry in walkdir::WalkDir::new(root).follow_links(false) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    if warnings.len() < 20 {
                        warnings.push(error.to_string());
                    }
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if !is_audio(path) {
                continue;
            }
            let canonical = match path.canonicalize() {
                Ok(path) => path,
                Err(_) => continue,
            };
            if !seen.insert(canonical.clone()) {
                continue;
            }
            paths.push(canonical);
            if paths.len() % 128 == 0 {
                progress(paths.len(), 0);
            }
        }
        progress(paths.len(), 0);
    }
    (paths, warnings)
}

fn is_audio(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    EXTENSIONS.contains(&extension.as_str())
}

fn read_track(path: &Path) -> Result<Track, String> {
    let mut track = Track {
        title: path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
        path: Some(path.to_path_buf()),
        ..Track::default()
    };
    if path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("ncm"))
    {
        let fail = |error: anyhow::Error| format!("{}: {error}", path.display());
        let mut info = File::open(path)
            .map_err(anyhow::Error::from)
            .and_then(|mut file| crate::ncm::metadata(&mut file))
            .map_err(fail)?;
        info.path = track.path;
        if info.title.is_empty() {
            info.title = track.title;
        }
        return Ok(info);
    }
    if let Ok(tagged) = Probe::open(path).and_then(|probe| {
        probe
            .options(ParseOptions::new().read_cover_art(false))
            .read()
    }) {
        track.duration_ms = tagged.properties().duration().as_millis() as u64;
        if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
            if let Some(title) = tag.title() {
                track.title = title.into_owned();
            }
            track.artist = tag.artist().map(|s| s.into_owned()).unwrap_or_default();
            track.album = tag.album().map(|s| s.into_owned()).unwrap_or_default();
        }
    }
    Ok(track)
}

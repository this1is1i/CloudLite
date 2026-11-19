use crate::model::Track;
use lofty::{
    config::ParseOptions,
    file::{AudioFile, TaggedFileExt},
    probe::Probe,
    tag::Accessor,
};
use std::path::PathBuf;
use std::{collections::HashSet, fs::File};

pub fn scan(roots: &[PathBuf]) -> (Vec<Track>, Vec<String>) {
    let mut tracks = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        for entry in walkdir::WalkDir::new(root).follow_links(false) {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    if warnings.len() < 20 {
                        warnings.push(e.to_string());
                    }
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ![
                "mp3", "flac", "wav", "m4a", "aac", "ogg", "opus", "wma", "ape", "ncm",
            ]
            .contains(&ext.as_str())
            {
                continue;
            }
            let canonical = match path.canonicalize() {
                Ok(p) => p,
                Err(_) => continue,
            };
            if !seen.insert(canonical.clone()) {
                continue;
            }
            let mut track = Track {
                title: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                path: Some(canonical),
                ..Track::default()
            };
            if ext == "ncm" {
                match File::open(path)
                    .map_err(anyhow::Error::from)
                    .and_then(|mut f| crate::ncm::metadata(&mut f))
                {
                    Ok(mut info) => {
                        info.path = track.path;
                        if info.title.is_empty() {
                            info.title = track.title;
                        }
                        track = info;
                    }
                    Err(e) => {
                        if warnings.len() < 20 {
                            warnings.push(format!("{}: {e}", path.display()));
                        }
                        continue;
                    }
                }
            } else if let Ok(tagged) = Probe::open(path)
                .and_then(|p| p.options(ParseOptions::new().read_cover_art(false)).read())
            {
                track.duration_ms = tagged.properties().duration().as_millis() as u64;
                if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
                    if let Some(title) = tag.title() {
                        track.title = title.into_owned();
                    }
                    track.artist = tag.artist().map(|s| s.into_owned()).unwrap_or_default();
                    track.album = tag.album().map(|s| s.into_owned()).unwrap_or_default();
                }
            }
            tracks.push(track);
        }
    }
    tracks.sort_by(|a, b| a.title.cmp(&b.title).then(a.artist.cmp(&b.artist)));
    (tracks, warnings)
}

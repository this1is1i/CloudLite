use cloudlite::{
    library,
    model::{local_match, parse_tracks, Queue, Track},
    persistence,
};
use serde_json::json;
use std::{fs, path::PathBuf};

fn track(
    id: Option<u64>,
    title: &str,
    artist: &str,
    album: &str,
    duration: u64,
    path: Option<&str>,
) -> Track {
    Track {
        id,
        title: title.into(),
        artist: artist.into(),
        album: album.into(),
        duration_ms: duration,
        path: path.map(PathBuf::from),
    }
}

#[test]
fn cloud_tracks_parse_both_search_and_playlist_shapes_without_losing_ids() {
    let parsed = parse_tracks(&json!([
        {"id":42,"name":"晴天","ar":[{"name":"周杰伦"}],"al":{"name":"叶惠美"},"dt":269000},
        {"id":43,"name":"夜曲","artists":[{"name":"周杰伦"}],"album":{"name":"十一月的萧邦"},"duration":226000}
    ]));
    assert_eq!(parsed.len(), 2);
    assert_eq!(
        parsed[0],
        track(Some(42), "晴天", "周杰伦", "叶惠美", 269000, None)
    );
    assert_eq!(parsed[1].album, "十一月的萧邦");
}

#[test]
fn exact_netease_id_wins_over_a_similar_local_title() {
    let dir = tempfile::tempdir().unwrap();
    let right = dir.path().join("right.ncm");
    let wrong = dir.path().join("wrong.flac");
    fs::write(&right, b"fixture").unwrap();
    fs::write(&wrong, b"fixture").unwrap();
    let remote = track(Some(42), "晴天", "周杰伦", "", 269000, None);
    let local = vec![
        track(Some(99), "晴天", "周杰伦", "", 269000, wrong.to_str()),
        track(Some(42), "different tag", "", "", 0, right.to_str()),
    ];
    assert_eq!(local_match(&remote, &local).unwrap().path, Some(right));
}

#[test]
fn metadata_match_rejects_live_versions_and_ambiguous_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("live.flac");
    let studio = dir.path().join("studio.flac");
    let other = dir.path().join("other.flac");
    for path in [&live, &studio, &other] {
        fs::write(path, b"fixture").unwrap();
    }
    let remote = track(Some(42), "晴天", "周杰伦", "叶惠美", 269000, None);
    let local = vec![
        track(None, "晴天 (Live)", "周杰伦", "", 269000, live.to_str()),
        track(None, "晴天", "周杰伦", "叶惠美", 269500, studio.to_str()),
    ];
    assert_eq!(local_match(&remote, &local).unwrap().path, Some(studio));
    let ambiguous = vec![
        local[1].clone(),
        track(None, "晴天", "周杰伦", "叶惠美", 269100, other.to_str()),
    ];
    assert!(local_match(&remote, &ambiguous).is_none());
    assert!(local_match(
        &remote,
        &[track(
            Some(77),
            "晴天",
            "周杰伦",
            "叶惠美",
            269000,
            Some("wrong-id.flac")
        )]
    )
    .is_none());
}

#[test]
fn unavailable_exact_id_does_not_hide_an_available_metadata_match() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("studio.flac");
    fs::write(&path, b"fixture").unwrap();
    let remote = track(Some(42), "晴天", "周杰伦", "叶惠美", 269000, None);
    let local = vec![
        track(
            Some(42),
            "晴天",
            "周杰伦",
            "叶惠美",
            269000,
            Some("missing.ncm"),
        ),
        track(None, "晴天", "周杰伦", "叶惠美", 269000, path.to_str()),
    ];
    assert_eq!(local_match(&remote, &local).unwrap().path, Some(path));
}

#[test]
fn queue_replacement_and_navigation_are_bounded() {
    let mut queue = Queue::default();
    assert!(queue.advance(1).is_none());
    queue.replace(
        vec![
            track(Some(1), "a", "", "", 0, None),
            track(Some(2), "b", "", "", 0, None),
        ],
        1,
    );
    assert!(
        queue.advance(1).is_none(),
        "end of queue must stop, not restart silently"
    );
    assert_eq!(queue.advance(-1).unwrap().id, Some(1));
    queue.replace(vec![], 0);
    assert_eq!(queue.index, None);
}

#[test]
fn local_scan_handles_unicode_nested_folders_and_overlapping_roots() {
    let dir = tempfile::tempdir().unwrap();
    let child = dir.path().join("周杰伦");
    fs::create_dir(&child).unwrap();
    fs::write(
        child.join("晴天.MP3"),
        b"not an mp3 but should be listed with a filename fallback",
    )
    .unwrap();
    fs::write(child.join("ignore.txt"), b"text").unwrap();
    let (tracks, _) = library::scan(&[dir.path().into(), child]);
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].title, "晴天");
    assert!(tracks[0].path.as_ref().unwrap().is_file());
}

#[test]
fn persistence_roundtrips_unicode_and_recovers_from_a_corrupt_current_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    persistence::save_json(&path, &json!({"folders":["D:\\音乐"],"version":1})).unwrap();
    persistence::save_json(&path, &json!({"folders":["E:\\收藏"],"version":2})).unwrap();
    assert_eq!(persistence::load_json(&path).unwrap()["version"], 2);
    fs::write(&path, b"{interrupted").unwrap();
    assert_eq!(persistence::load_json(&path).unwrap()["version"], 1);
}

#[test]
fn shuffle_plays_every_track_once_per_pass_without_immediate_repeats() {
    let mut queue = Queue::default();
    queue.replace(
        (0..6)
            .map(|i| track(Some(i), "t", "a", "", 0, None))
            .collect(),
        0,
    );
    queue.set_shuffle(true);
    let mut first = Vec::new();
    for _ in 0..5 {
        first.push(queue.advance(1).unwrap().id.unwrap());
    }
    first.sort_unstable();
    assert_eq!(
        first,
        vec![1, 2, 3, 4, 5],
        "the first pass must cover every other track exactly once"
    );
    let mut previous = queue.index.unwrap() as u64;
    for _ in 0..18 {
        let next = queue.advance(1).unwrap().id.unwrap();
        assert_ne!(next, previous, "shuffle must not repeat the current track");
        previous = next;
    }
}

#[test]
fn shuffle_previous_walks_back_and_a_single_track_stops() {
    let mut queue = Queue::default();
    queue.replace(
        (0..3)
            .map(|i| track(Some(i), "t", "a", "", 0, None))
            .collect(),
        0,
    );
    queue.set_shuffle(true);
    let first = queue.advance(1).unwrap().id.unwrap();
    queue.advance(1).unwrap();
    assert_eq!(queue.advance(-1).unwrap().id, Some(first));
    assert_eq!(queue.advance(-1).unwrap().id, Some(0));
    assert!(queue.advance(-1).is_none());
    queue.replace(vec![track(Some(9), "only", "", "", 0, None)], 0);
    assert!(queue.shuffle(), "replacing the queue keeps the mode");
    assert!(
        queue.advance(1).is_none(),
        "a single shuffled track stops instead of looping"
    );
}

use cloudlite::{
    cloud::*,
    model::{parse_tracks, Track},
};
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn cookies_merge_rotations_without_saving_set_cookie_attributes() {
    let mut cookies = BTreeMap::from([
        ("MUSIC_U".into(), "old".into()),
        ("__csrf".into(), "retain".into()),
    ]);
    merge_cookies(
        &mut cookies,
        &[
            "MUSIC_U=new=token; Path=/; HttpOnly; Secure; Max-Age=100".into(),
            "NMTID=abc; Expires=Tue, 01 Jan 2030 00:00:00 GMT".into(),
        ],
    );
    assert_eq!(cookies["MUSIC_U"], "new=token");
    assert_eq!(cookies["__csrf"], "retain");
    assert_eq!(cookies.len(), 3);
    assert!(!cookies.contains_key("Path"));
    merge_cookies(&mut cookies, &["MUSIC_U=; Max-Age=0; Path=/".into()]);
    assert!(!cookies.contains_key("MUSIC_U"));
}

#[test]
fn a_large_playlist_uses_all_track_ids_and_preserves_remote_order() {
    let ids=playlist_ids(&json!({"playlist":{"trackCount":3,"tracks":[{"id":1}],"trackIds":[{"id":3},{"id":1},{"id":2}]}})).unwrap();
    assert_eq!(ids, vec![3, 1, 2]);
    let songs =
        parse_tracks(&json!([{"id":1,"name":"one"},{"id":2,"name":"two"},{"id":3,"name":"three"}]));
    assert_eq!(
        order_tracks(&ids, songs)
            .iter()
            .map(|s| s.id.unwrap())
            .collect::<Vec<_>>(),
        ids
    );
    assert!(
        playlist_ids(&json!({"playlist":{"trackCount":2,"trackIds":[{"id":1}]}})).is_err(),
        "a partial response must not be presented as a full playlist"
    );
}

#[test]
fn restricted_tracks_fail_explicitly_and_previews_are_identified() {
    assert!(playback_url(&json!({"data":[{"url":null,"code":-110}]})).is_err());
    let (url,preview)=playback_url(&json!({"data":[{"url":"http://m801.music.126.net/song.mp3","freeTrialInfo":{"start":0,"end":30}}]})).unwrap();
    assert!(url.starts_with("https://"));
    assert!(preview);
    assert!(playback_url(&json!({"data":[{"url":"file:///C:/secret"}]})).is_err());
    let _ = Track::default();
}

#[test]
fn isolated_picker_protocol_handles_unicode_cancellation_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("音乐");
    std::fs::create_dir(&path).unwrap();
    let output = serde_json::to_vec(&serde_json::json!({"path":path})).unwrap();
    assert_eq!(
        cloudlite::picker::decode_result(&output).unwrap(),
        Some(path)
    );
    assert_eq!(
        cloudlite::picker::decode_result(br#"{"path":null}"#).unwrap(),
        None
    );
    assert!(cloudlite::picker::decode_result(br#"{"path":"relative"}"#).is_err());
    assert!(cloudlite::picker::decode_result(br#"{"error":"picker could not open"}"#).is_err());
    assert!(cloudlite::picker::decode_result(b"not json").is_err());
}

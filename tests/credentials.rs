#[test]
fn session_credentials_are_bound_to_windows_user_and_not_stored_as_plaintext() {
    let plain = b"MUSIC_U=dummy-test-token; __csrf=dummy";
    let protected = cloudlite::credentials::protect(plain).unwrap();
    assert!(!protected.windows(plain.len()).any(|s| s == plain));
    assert_eq!(
        cloudlite::credentials::unprotect(&protected).unwrap(),
        plain
    );
    assert!(cloudlite::credentials::unprotect(b"invalid ciphertext").is_err());
}

#[test]
fn starting_and_cancelling_a_new_login_preserves_the_existing_session() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("session.dpapi");
    let existing = cloudlite::credentials::protect(br#"{"MUSIC_U":"dummy-old-token"}"#).unwrap();
    std::fs::write(&session, &existing).unwrap();
    let candidate = cloudlite::cloud::CloudClient::sign_in(dir.path(), "unit-test-device");
    assert!(
        !candidate.has_session(),
        "a new sign-in must not inherit the active account"
    );
    drop(candidate);
    assert_eq!(
        std::fs::read(session).unwrap(),
        existing,
        "cancelled sign-in must leave active credentials untouched"
    );
}

use aes::{
    cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyInit},
    Aes128,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use cloudlite::ncm;
use std::io::{Cursor, Read, Seek, SeekFrom};

fn aes_ecb(input: &[u8], key: &[u8; 16]) -> Vec<u8> {
    ecb::Encryptor::<Aes128>::new(key.into()).encrypt_padded_vec_mut::<Pkcs7>(input)
}
fn chunk(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend((bytes.len() as u32).to_le_bytes());
    output.extend(bytes);
}
fn fixture(audio: &[u8]) -> Vec<u8> {
    let key = b"fixture-stream-key";
    let mut output = b"CTENFDAM\0\0".to_vec();
    let key_chunk = aes_ecb(
        &[b"neteasecloudmusic".as_slice(), key].concat(),
        b"hzHRAmso5kInbaxW",
    )
    .iter()
    .map(|b| b ^ 0x64)
    .collect::<Vec<_>>();
    chunk(&mut output, &key_chunk);
    let meta = br#"music:{"musicId":42,"musicName":"test","artist":[["artist",1]],"album":"album","duration":2500,"format":"flac"}"#;
    let encoded = STANDARD.encode(aes_ecb(
        meta,
        &[
            0x23, 0x31, 0x34, 0x6c, 0x6a, 0x6b, 0x5f, 0x21, 0x5c, 0x5d, 0x26, 0x30, 0x55, 0x3c,
            0x27, 0x28,
        ],
    ));
    let meta_chunk = [b"163 key(Don't modify):".as_slice(), encoded.as_bytes()]
        .concat()
        .iter()
        .map(|b| b ^ 0x63)
        .collect::<Vec<_>>();
    chunk(&mut output, &meta_chunk);
    output.extend(0_u32.to_le_bytes());
    output.push(0); // CRC32 + image version
    output.extend(7_u32.to_le_bytes()); // allocated cover space
    output.extend(3_u32.to_le_bytes()); // actual cover bytes
    output.extend(b"img\0\0\0\0");
    let mut box_ = [0_u8; 256];
    for (i, v) in box_.iter_mut().enumerate() {
        *v = i as u8;
    }
    let mut j = 0_usize;
    for i in 0..256 {
        j = (j + box_[i] as usize + key[i % key.len()] as usize) & 255;
        box_.swap(i, j);
    }
    for (i, b) in audio.iter().enumerate() {
        let j = (i + 1) & 255;
        let k = box_[(box_[j] as usize + box_[(box_[j] as usize + j) & 255] as usize) & 255];
        output.push(*b ^ k);
    }
    output
}

#[test]
fn ncm_decode_preserves_exact_audio_bytes_across_buffer_boundaries() {
    let audio: Vec<u8> = b"fLaC"
        .iter()
        .copied()
        .chain((0..200_000).map(|i| (i % 251) as u8))
        .collect();
    let input = fixture(&audio);
    let mut decoded = Vec::new();
    assert_eq!(
        ncm::decode(&mut Cursor::new(input), &mut decoded).unwrap(),
        "flac"
    );
    assert_eq!(decoded, audio);
}

#[test]
fn ncm_metadata_reads_identity_without_decoding_audio() {
    let input = fixture(b"fLaCpayload");
    let mut input = Cursor::new(input);
    let metadata = ncm::metadata(&mut input).unwrap();
    assert_eq!(metadata.id, Some(42));
    assert_eq!(metadata.title, "test");
    assert_eq!(metadata.artist, "artist");
    assert_eq!(metadata.duration_ms, 2500);
}

#[test]
fn malformed_ncm_is_rejected_without_unbounded_allocation_or_partial_audio() {
    let mut bad = b"CTENFDAM\0\0".to_vec();
    bad.extend(u32::MAX.to_le_bytes());
    let mut decoded = Vec::new();
    assert!(ncm::decode(&mut Cursor::new(bad), &mut decoded).is_err());
    assert!(decoded.is_empty());
    assert!(ncm::metadata(&mut Cursor::new(b"not an ncm")).is_err());
    let mut truncated = fixture(b"fLaCabc");
    truncated.truncate(20);
    assert!(ncm::decode(&mut Cursor::new(truncated), &mut decoded).is_err());
}

struct BoundedReader(Cursor<Vec<u8>>);
impl Read for BoundedReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        assert!(
            buffer.len() <= 65536,
            "audio must stream through a bounded buffer"
        );
        self.0.read(buffer)
    }
}
impl Seek for BoundedReader {
    fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(p)
    }
}

#[test]
fn decoding_large_files_keeps_read_buffers_bounded() {
    let audio: Vec<u8> = b"fLaC"
        .iter()
        .copied()
        .chain(std::iter::repeat_n(17, 1_000_000))
        .collect();
    let mut input = BoundedReader(Cursor::new(fixture(&audio)));
    ncm::decode(&mut input, &mut std::io::sink()).unwrap();
}

#[test]
fn playback_preparation_leaves_original_untouched_and_removes_its_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("原曲.ncm");
    let input = fixture(b"fLaCexample");
    std::fs::write(&source, &input).unwrap();
    let prepared = ncm::prepare(&source, dir.path()).unwrap();
    let path = prepared.path().to_path_buf();
    assert_eq!(path.extension().unwrap(), "flac");
    assert_eq!(std::fs::read(&path).unwrap(), b"fLaCexample");
    assert_eq!(std::fs::read(&source).unwrap(), input);
    drop(prepared);
    assert!(!path.exists());
    assert!(source.exists());
}

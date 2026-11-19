use crate::model::Track;
use aes::{
    cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyInit},
    Aes128,
};
use anyhow::{bail, ensure, Context};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::io::{Read, Seek, SeekFrom, Write};

const CORE_KEY: &[u8; 16] = b"hzHRAmso5kInbaxW";
const META_KEY: [u8; 16] = [
    0x23, 0x31, 0x34, 0x6c, 0x6a, 0x6b, 0x5f, 0x21, 0x5c, 0x5d, 0x26, 0x30, 0x55, 0x3c, 0x27, 0x28,
];

pub fn prepare(
    source: &std::path::Path,
    cache: &std::path::Path,
) -> anyhow::Result<tempfile::NamedTempFile> {
    let mut input = std::fs::File::open(source)?;
    let h = header(&mut input)?;
    let mut prefix = [0; 16];
    let count = input.read(&mut prefix)?;
    for (i, b) in prefix[..count].iter_mut().enumerate() {
        let j = (i + 1) & 255;
        *b ^= h.key_box
            [(h.key_box[j] as usize + h.key_box[(h.key_box[j] as usize + j) & 255] as usize) & 255];
    }
    let format = sniff(&prefix[..count])?;
    std::fs::create_dir_all(cache)?;
    let mut output = tempfile::Builder::new()
        .prefix("cloudlite-ncm-")
        .suffix(&format!(".{format}"))
        .tempfile_in(cache)?;
    decode(&mut input, &mut output)?;
    output.flush()?;
    Ok(output)
}

fn sniff(buffer: &[u8]) -> anyhow::Result<&'static str> {
    Ok(if buffer.starts_with(b"fLaC") {
        "flac"
    } else if buffer.starts_with(b"ID3")
        || (buffer.len() >= 2 && buffer[0] == 0xff && buffer[1] & 0xe0 == 0xe0)
    {
        "mp3"
    } else if buffer.len() >= 8 && &buffer[4..8] == b"ftyp" {
        "m4a"
    } else if buffer.starts_with(b"RIFF") {
        "wav"
    } else {
        bail!("NCM 内音频格式无法识别")
    })
}

fn number(reader: &mut impl Read) -> anyhow::Result<u32> {
    let mut buf = [0; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}
fn chunk(reader: &mut impl Read, limit: usize) -> anyhow::Result<Vec<u8>> {
    let len = number(reader)? as usize;
    ensure!(len <= limit, "NCM 数据块过大");
    let mut data = vec![0; len];
    reader.read_exact(&mut data)?;
    Ok(data)
}
fn decrypt(data: &[u8], key: &[u8; 16]) -> anyhow::Result<Vec<u8>> {
    ecb::Decryptor::<Aes128>::new(key.into())
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .map_err(|_| anyhow::anyhow!("NCM 密钥或元数据损坏"))
}
struct Header {
    track: Track,
    key_box: [u8; 256],
    audio_start: u64,
}

fn header<R: Read + Seek>(reader: &mut R) -> anyhow::Result<Header> {
    reader.seek(SeekFrom::Start(0))?;
    let mut magic = [0; 8];
    reader.read_exact(&mut magic)?;
    ensure!(&magic == b"CTENFDAM", "不是有效的 NCM 文件");
    reader.seek(SeekFrom::Current(2))?;
    let key_data = chunk(reader, 4096)?
        .into_iter()
        .map(|b| b ^ 0x64)
        .collect::<Vec<_>>();
    let key = decrypt(&key_data, CORE_KEY)?;
    let key = key
        .strip_prefix(b"neteasecloudmusic")
        .context("NCM 密钥前缀无效")?;
    ensure!(!key.is_empty(), "NCM 密钥为空");
    let mut key_box = [0u8; 256];
    for (i, v) in key_box.iter_mut().enumerate() {
        *v = i as u8;
    }
    let mut j = 0usize;
    for i in 0..256 {
        j = (j + key_box[i] as usize + key[i % key.len()] as usize) & 255;
        key_box.swap(i, j);
    }
    let meta = chunk(reader, 65536)?
        .into_iter()
        .map(|b| b ^ 0x63)
        .collect::<Vec<_>>();
    let mut track = Track::default();
    if !meta.is_empty() {
        let encoded = meta
            .strip_prefix(b"163 key(Don't modify):")
            .context("不支持的 NCM 元数据版本")?;
        let value = decrypt(&STANDARD.decode(encoded)?, &META_KEY)?;
        let json = value
            .strip_prefix(b"music:")
            .or_else(|| value.strip_prefix(b"dj:"))
            .context("NCM 元数据前缀无效")?;
        let v: serde_json::Value = serde_json::from_slice(json)?;
        track.id = v["musicId"]
            .as_u64()
            .or_else(|| v["musicId"].as_str().and_then(|s| s.parse().ok()));
        track.title = v["musicName"].as_str().unwrap_or("").into();
        track.album = v["album"].as_str().unwrap_or("").into();
        track.duration_ms = v["duration"].as_u64().unwrap_or(0);
        track.artist = v["artist"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x[0].as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .unwrap_or_default();
    }
    reader.seek(SeekFrom::Current(5))?; // CRC32 (4) + cover version (1)
    let cover_frame = number(reader)? as u64;
    let cover_len = number(reader)? as u64;
    ensure!(
        cover_len <= cover_frame && cover_frame <= 64 * 1024 * 1024,
        "NCM 封面长度无效"
    );
    let audio_start = reader
        .stream_position()?
        .checked_add(cover_frame)
        .context("NCM 偏移溢出")?;
    let end = reader.seek(SeekFrom::End(0))?;
    ensure!(audio_start < end, "NCM 音频为空或文件被截断");
    reader.seek(SeekFrom::Start(audio_start))?;
    Ok(Header {
        track,
        key_box,
        audio_start,
    })
}

pub fn metadata<R: Read + Seek>(reader: &mut R) -> anyhow::Result<Track> {
    Ok(header(reader)?.track)
}

/// Decode into an application-owned temporary file. Audio memory is bounded to 64 KiB;
/// cover images are skipped and the source file is never changed.
pub fn decode<R: Read + Seek, W: Write>(reader: &mut R, writer: &mut W) -> anyhow::Result<String> {
    let h = header(reader)?;
    reader.seek(SeekFrom::Start(h.audio_start))?;
    let mut buffer = [0u8; 65536];
    let mut offset = 0usize;
    let mut format = String::new();
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        for (i, b) in buffer[..count].iter_mut().enumerate() {
            let j = (offset + i + 1) & 255;
            *b ^= h.key_box[(h.key_box[j] as usize
                + h.key_box[(h.key_box[j] as usize + j) & 255] as usize)
                & 255];
        }
        if offset == 0 {
            format = sniff(&buffer[..count])?.into();
        }
        writer.write_all(&buffer[..count])?;
        offset += count;
    }
    ensure!(offset > 0, "NCM 没有音频");
    Ok(format)
}

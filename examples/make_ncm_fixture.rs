//! Development-only encoder for synthetic, freely generated test audio.
use aes::{
    cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyInit},
    Aes128,
};
use base64::{engine::general_purpose::STANDARD, Engine};
fn encrypt(data: &[u8], key: &[u8; 16]) -> Vec<u8> {
    ecb::Encryptor::<Aes128>::new(key.into()).encrypt_padded_vec_mut::<Pkcs7>(data)
}
fn chunk(output: &mut Vec<u8>, data: &[u8]) {
    output.extend((data.len() as u32).to_le_bytes());
    output.extend(data);
}
fn main() -> anyhow::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    anyhow::ensure!(args.len() == 3, "make_ncm_fixture INPUT.flac OUTPUT.ncm");
    let audio = std::fs::read(&args[1])?;
    anyhow::ensure!(audio.starts_with(b"fLaC"), "expected synthetic FLAC");
    let key = b"cloudlite-synthetic-audio";
    let mut output = b"CTENFDAM\0\0".to_vec();
    let encrypted = encrypt(
        &[b"neteasecloudmusic".as_slice(), key].concat(),
        b"hzHRAmso5kInbaxW",
    )
    .iter()
    .map(|b| b ^ 0x64)
    .collect::<Vec<_>>();
    chunk(&mut output, &encrypted);
    let metadata = serde_json::json!({"musicId":42,"musicName":"静音测试 NCM","artist":[["CloudLite",1]],"album":"本地验收","duration":45000,"format":"flac"});
    let data = format!("music:{metadata}");
    let encrypted = encrypt(
        data.as_bytes(),
        &[
            0x23, 0x31, 0x34, 0x6c, 0x6a, 0x6b, 0x5f, 0x21, 0x5c, 0x5d, 0x26, 0x30, 0x55, 0x3c,
            0x27, 0x28,
        ],
    );
    let encoded = format!("163 key(Don't modify):{}", STANDARD.encode(encrypted))
        .bytes()
        .map(|b| b ^ 0x63)
        .collect::<Vec<_>>();
    chunk(&mut output, &encoded);
    output.extend([0; 5]);
    output.extend(0_u32.to_le_bytes());
    output.extend(0_u32.to_le_bytes());
    let mut box_ = [0u8; 256];
    for (i, v) in box_.iter_mut().enumerate() {
        *v = i as u8;
    }
    let mut j = 0usize;
    for i in 0..256 {
        j = (j + box_[i] as usize + key[i % key.len()] as usize) & 255;
        box_.swap(i, j);
    }
    for (i, byte) in audio.iter().enumerate() {
        let j = (i + 1) & 255;
        output.push(
            byte ^ box_[(box_[j] as usize + box_[(box_[j] as usize + j) & 255] as usize) & 255],
        );
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[2])
        .and_then(|mut f| std::io::Write::write_all(&mut f, &output))?;
    Ok(())
}

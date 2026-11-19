use windows::{
    core::w,
    Win32::{
        Foundation::{LocalFree, HLOCAL},
        Security::Cryptography::*,
    },
};

pub fn protect(plain: &[u8]) -> anyhow::Result<Vec<u8>> {
    transform(plain, true)
}
pub fn unprotect(encrypted: &[u8]) -> anyhow::Result<Vec<u8>> {
    transform(encrypted, false)
}
fn transform(input: &[u8], encrypt: bool) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(input.len() <= 1_048_576, "凭据长度异常");
    let input = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                w!("CloudLite session"),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )?;
        } else {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )?;
        }
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
        Ok(bytes)
    }
}

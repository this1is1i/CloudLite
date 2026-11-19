//! Embeds the application manifest and the multi-size icon into the executable.
//! The icon resource is compiled with the Windows SDK resource compiler (rc.exe);
//! when it is unavailable the build still succeeds but the executable keeps the
//! default icon, and the build prints a warning.
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    let manifest = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("app.manifest");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rustc-link-arg-bin=cloudlite=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bin=cloudlite=/MANIFESTINPUT:{}",
        manifest.display()
    );

    let icon = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/cloudlite.ico");
    println!("cargo:rerun-if-changed=assets/cloudlite.ico");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    match embed_icon(&icon, &out) {
        Some(res) => println!("cargo:rustc-link-arg-bin=cloudlite={}", res.display()),
        None => println!("cargo:warning=rc.exe 未找到，可执行文件将不包含图标资源"),
    }
}

fn embed_icon(icon: &Path, out: &Path) -> Option<PathBuf> {
    let compiler = find_resource_compiler()?;
    let script = out.join("cloudlite.rc");
    let resource = out.join("cloudlite.res");
    // IDI_ICON1 = 1: the id the program passes to LoadImageW at run time.
    std::fs::write(
        &script,
        format!(
            "1 ICON \"{}\"\n",
            icon.display().to_string().replace('\\', "/")
        ),
    )
    .ok()?;
    let status = Command::new(&compiler)
        .arg("/nologo")
        .arg("/fo")
        .arg(&resource)
        .arg(&script)
        .status()
        .ok()?;
    status.success().then_some(resource)
}

fn find_resource_compiler() -> Option<PathBuf> {
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let kits = [
        env::var_os("ProgramFiles(x86)").map(PathBuf::from),
        env::var_os("ProgramFiles").map(PathBuf::from),
    ];
    let mut found: Vec<(u64, PathBuf)> = Vec::new();
    for base in kits.into_iter().flatten() {
        let bin = base.join("Windows Kits/10/bin");
        let Ok(entries) = std::fs::read_dir(&bin) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path().join("x64/rc.exe");
            if !candidate.is_file() {
                continue;
            }
            let key = entry
                .file_name()
                .to_string_lossy()
                .split('.')
                .filter_map(|part| part.parse::<u64>().ok())
                .fold(0u64, |acc, part| {
                    acc.saturating_mul(1000).saturating_add(part)
                });
            found.push((key, candidate));
        }
    }
    found.sort_by_key(|(key, _)| *key);
    found.pop().map(|(_, path)| path)
}

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn rc_in_sdk_bin(root: &Path, host_arch: &str) -> Option<PathBuf> {
    let direct = root.join(host_arch).join("rc.exe");
    if direct.is_file() {
        return Some(direct);
    }
    let mut versions: Vec<_> = fs::read_dir(root).ok()?.flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let version: Option<Vec<u32>> = name.split('.').map(|part| part.parse().ok()).collect();
            version.map(|version| (version, entry.path()))
        })
        .collect();
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    versions.into_iter().map(|(_, path)| path.join(host_arch).join("rc.exe"))
        .find(|path| path.is_file())
}

fn find_rc() -> Option<PathBuf> {
    if let Some(paths) = env::var_os("PATH") {
        if let Some(rc) = env::split_paths(&paths).map(|path| path.join("rc.exe")).find(|path| path.is_file()) {
            return Some(rc);
        }
    }
    let host = env::var("HOST").unwrap_or_default();
    let arch = if host.starts_with("aarch64") { "arm64" } else if host.starts_with("i686") { "x86" } else { "x64" };
    let mut roots = Vec::new();
    if let Some(path) = env::var_os("WindowsSdkVerBinPath") {
        roots.push(PathBuf::from(path));
    }
    if let Some(path) = env::var_os("WindowsSdkDir") {
        roots.push(PathBuf::from(path).join("bin"));
    }
    if let Some(path) = env::var_os("ProgramFiles(x86)") {
        roots.push(PathBuf::from(path).join("Windows Kits").join("10").join("bin"));
    }
    roots.into_iter().find_map(|root| rc_in_sdk_bin(&root, arch))
}

fn main() {
    println!("cargo:rerun-if-changed=assets/ReplyJP.ico");
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-env-changed=WindowsSdkDir");
    println!("cargo:rerun-if-env-changed=WindowsSdkVerBinPath");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        panic!("ReplyJP requires the Windows MSVC Rust target");
    }

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    fs::copy("assets/ReplyJP.ico", out.join("ReplyJP.ico")).expect("failed to copy the application icon");
    fs::write(out.join("ReplyJP.rc"), "1 ICON \"ReplyJP.ico\"\n").expect("failed to write the resource script");
    let rc = find_rc().expect("Windows SDK resource compiler (rc.exe) not found");
    let result = Command::new(rc).current_dir(&out)
        .args(["/nologo", "/fo", "ReplyJP.res", "ReplyJP.rc"])
        .output().expect("failed to run the Windows resource compiler");
    if !result.status.success() {
        panic!("resource compilation failed: {}{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
    }
    println!("cargo:rustc-link-arg-bin=ReplyJP={}", out.join("ReplyJP.res").display());
}

use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const FEED_URL: &str = "https://github.com/ggml-org/llama.cpp/releases.atom";
const POINTER_NAME: &str = ".reply-llama-runtime";
const PENDING_POINTER_NAME: &str = ".reply-llama-pending";
const MANIFEST_NAME: &str = ".reply-runtime-manifest";
const LLAMA_LICENSE: &[u8] = include_bytes!("../licenses/llama.cpp-MIT.txt");
const MAX_METADATA: u64 = 4 * 1024 * 1024;
const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;
const MAX_EXTRACTED: u64 = 512 * 1024 * 1024;
const MAX_FILE: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimeTag { build: u32 }

impl RuntimeTag {
    fn tag(&self) -> String { format!("b{}", self.build) }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Release { pub build: u32, sha256: String }

impl Release {
    fn tag(&self) -> RuntimeTag { RuntimeTag { build: self.build } }
}

#[cfg(test)]
pub(crate) fn test_release(build: u32) -> Release { Release { build, sha256: "0".repeat(64) } }

#[derive(Debug)]
pub(crate) struct Check { pub installed: u32, pub available: Option<Release> }

fn parse_tag(tag: &str) -> Option<RuntimeTag> {
    let build = tag.strip_prefix('b')?;
    if build.is_empty() || build.len() > 10 || !build.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(RuntimeTag { build: build.parse().ok()? })
}

fn tag_from_directory(name: &str) -> Option<RuntimeTag> {
    let (tag, nonce) = name.strip_prefix("runtime-vulkan-")?.rsplit_once('-')?;
    if nonce.len() != 16 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) { return None; }
    parse_tag(tag)
}

fn runtime_name_is_valid(name: &str) -> bool {
    tag_from_directory(name).is_some()
}

fn safe_archive_name(name: &str) -> bool {
    if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        || name.contains("..") { return false; }
    let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
    if matches!(stem.as_str(), "con" | "prn" | "aux" | "nul") { return false; }
    if stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt"))
        && matches!(stem.as_bytes()[3], b'1'..=b'9') { return false; }
    true
}

fn pointed_runtime(base: &Path, pointer: &str) -> Option<PathBuf> {
    let Ok(value) = fs::read_to_string(base.join(pointer)) else { return None; };
    let name = value.trim();
    if !runtime_name_is_valid(name) { return None; }
    let candidate = base.join(name);
    let Ok(meta) = fs::symlink_metadata(&candidate) else { return None; };
    if !meta.is_dir() || meta.file_type().is_symlink() || !candidate.join("llama-server.exe").is_file() { return None; }
    let (Ok(root), Ok(actual)) = (base.canonicalize(), candidate.canonicalize()) else { return None; };
    if actual.parent() != Some(root.as_path()) { return None; }
    Some(candidate)
}

pub(crate) fn selected_runtime(base: &Path) -> PathBuf {
    pointed_runtime(base, POINTER_NAME).unwrap_or_else(|| base.join("runtime-vulkan"))
}

fn needs_bootstrap_in(base: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(base.join(POINTER_NAME)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(format!("エンジンの設定を確認できない: {error}")),
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
            if pointed_runtime(base, POINTER_NAME).is_some() { Ok(false) }
            else { Err("エンジンの設定が不正である。設定ファイルを確認してほしい。".into()) }
        }
        Ok(_) => Err("エンジンの設定が通常のファイルではない。".into()),
    }
}

pub(crate) fn needs_bootstrap() -> Result<bool, String> {
    needs_bootstrap_in(&super::app_directory()?)
}

pub(crate) fn pending_runtime(base: &Path) -> Option<PathBuf> {
    pointed_runtime(base, PENDING_POINTER_NAME)
}

pub(crate) fn pending_build() -> Result<Option<u32>, String> {
    let base = super::app_directory()?;
    Ok(pending_runtime(&base).and_then(|path| {
        let name = path.file_name()?.to_str()?;
        Some(tag_from_directory(name)?.build)
    }))
}

fn pending_matches_in(base: &Path, release: &Release) -> bool {
    pending_runtime(base).and_then(|path| path.file_name()?.to_str().and_then(tag_from_directory))
        .is_some_and(|tag| tag == release.tag())
}

pub(crate) fn pending_matches(release: &Release) -> Result<bool, String> {
    Ok(pending_matches_in(&super::app_directory()?, release))
}

// A live session holds this read handle without delete sharing. Cleanup can
// therefore leave an older runtime intact when another window still uses it.
pub(crate) fn protect_runtime(server: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ
    }
    options.open(server).map_err(|error| format!("推論エンジンを保護できない: {error}"))
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("エンジンのファイルを確認できない: {error}"))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| format!("エンジンのファイルを確認できない: {error}"))?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn write_manifest(stage: &mut StagedRuntime) -> Result<(), String> {
    let mut hashes = serde_json::Map::new();
    for path in &stage.created_files {
        if !path.is_file() { continue; } // The downloaded ZIP was removed after extraction.
        let name = path.file_name().and_then(|name| name.to_str()).ok_or("エンジンのファイル名が不正である。")?;
        if !safe_archive_name(name) { return Err("エンジンのファイル名が不正である。".into()); }
        hashes.insert(name.to_owned(), serde_json::Value::String(hash_file(path)?));
    }
    let path = stage.directory.join(MANIFEST_NAME);
    let mut file = OpenOptions::new().write(true).create_new(true).open(&path)
        .map_err(|error| format!("エンジンの記録を保存できない: {error}"))?;
    stage.created_files.push(path);
    serde_json::to_writer(&mut file, &hashes).map_err(|error| format!("エンジンの記録を保存できない: {error}"))?;
    file.flush().map_err(|error| format!("エンジンの記録を保存できない: {error}"))
}

fn parse_build(text: &str) -> Option<u32> {
    let rest = text.split_once("build ")?.1;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() { None } else { digits.parse().ok() }
}

fn engine_build(server: &Path) -> Result<u32, String> {
    let mut command = Command::new(server);
    command.arg("--version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().map_err(|error| format!("エンジンの版を確認できない: {error}"))?;
    if !output.status.success() { return Err("エンジンの版を確認できない。".into()); }
    let text = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    parse_build(&text).ok_or_else(|| "エンジンの版番号を取得できない。".into())
}

fn validate_engine_identity(server: &Path, tag: &RuntimeTag) -> Result<(), String> {
    if engine_build(server)? != tag.build {
        return Err("エンジンの版番号が配布情報と一致しない。".into());
    }
    Ok(())
}

fn installed_tag(base: &Path) -> Result<RuntimeTag, String> {
    let runtime = pointed_runtime(base, POINTER_NAME).ok_or("推論エンジンを確認できない。")?;
    let tag = runtime.file_name().and_then(|name| name.to_str()).and_then(tag_from_directory)
        .ok_or("エンジンの版番号を取得できない。")?;
    validate_engine_identity(&runtime.join("llama-server.exe"), &tag)?;
    Ok(tag)
}

pub(crate) fn installed_build() -> Result<u32, String> {
    let base = super::app_directory()?;
    Ok(installed_tag(&base)?.build)
}

fn parse_feed(feed: &str) -> Vec<RuntimeTag> {
    let prefix = "https://github.com/ggml-org/llama.cpp/releases/tag/";
    let mut tags: Vec<RuntimeTag> = feed.split("<entry>").skip(1).filter_map(|entry| {
        let rest = entry.split_once(prefix)?.1;
        let tag = rest.split(|character: char| matches!(character, '"' | '\'' | '<' | '>' | ' ' | '\r' | '\n')).next()?;
        parse_tag(tag)
    }).collect();
    tags.sort_by(|a, b| b.build.cmp(&a.build));
    let mut seen = HashSet::new();
    tags.retain(|tag| seen.insert(tag.tag()));
    tags
}

fn parse_asset_digest(html: &str, tag: &RuntimeTag) -> Result<Option<String>, String> {
    let tag = tag.tag();
    let name = format!("llama-{tag}-bin-win-vulkan-x64.zip");
    let link = format!("/ggml-org/llama.cpp/releases/download/{tag}/{name}");
    let Some(start) = html.find(&link) else { return Ok(None); };
    let tail = &html[start..];
    let end = tail.find("</li>").ok_or("配布ファイルの情報を確認できない。")?;
    let block = &tail[..end];
    let hash_start = block.find("sha256:").ok_or("配布ファイルの SHA-256 を確認できない。")? + 7;
    let digest = block.get(hash_start..hash_start + 64).ok_or("配布ファイルの SHA-256 が不正である。")?;
    if !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) { return Err("配布ファイルの SHA-256 が不正である。".into()); }
    Ok(Some(digest.to_ascii_lowercase()))
}

fn network_client(timeout: Duration) -> Result<Client, String> {
    Client::builder().https_only(true).connect_timeout(Duration::from_secs(10))
        .timeout(timeout).redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(concat!("ReplyJP/", env!("CARGO_PKG_VERSION")))
        .build().map_err(|error| format!("更新確認の準備に失敗した: {error}"))
}

fn fetch_text(client: &Client, url: &str) -> Result<String, String> {
    let response = client.get(url).send().map_err(|error| format!("更新情報を取得できない: {}", error.without_url()))?;
    if !response.status().is_success() { return Err(format!("更新情報を取得できない（HTTP {}）。", response.status())); }
    if response.content_length().is_some_and(|length| length > MAX_METADATA) { return Err("更新情報が大きすぎる。".into()); }
    let mut bytes = Vec::new();
    response.take(MAX_METADATA + 1).read_to_end(&mut bytes).map_err(|error| format!("更新情報を読み取れない: {error}"))?;
    if bytes.len() as u64 > MAX_METADATA { return Err("更新情報が大きすぎる。".into()); }
    String::from_utf8(bytes).map_err(|_| "更新情報の文字形式が不正である。".into())
}

fn find_newer(installed: Option<&RuntimeTag>, feed: &str, mut fetch_asset: impl FnMut(&RuntimeTag) -> Result<String, String>) -> Result<Option<Release>, String> {
    let tags = parse_feed(feed);
    if tags.is_empty() { return Err("更新一覧から版番号を取得できない。".into()); }
    let newer: Vec<&RuntimeTag> = tags.iter().filter(|tag| installed.is_none_or(|current| tag.build > current.build)).collect();
    for tag in newer.iter().copied().take(12) {
        let html = fetch_asset(tag)?;
        if let Some(sha256) = parse_asset_digest(&html, tag)? {
            return Ok(Some(Release { build: tag.build, sha256 }));
        }
    }
    if newer.len() > 12 { return Err("更新候補が多く、利用可能な版を確認できない。".into()); }
    Ok(None)
}

fn remote_release_after(installed: Option<&RuntimeTag>) -> Result<Option<Release>, String> {
    let client = network_client(Duration::from_secs(30))?;
    let feed = fetch_text(&client, FEED_URL)?;
    find_newer(installed, &feed, |tag| {
        fetch_text(&client, &format!("https://github.com/ggml-org/llama.cpp/releases/expanded_assets/{}", tag.tag()))
    })
}

pub(crate) fn latest_available() -> Result<Release, String> {
    remote_release_after(None)?.ok_or_else(|| "対応する Windows 向け Vulkan 版のエンジンを取得できない。".into())
}

pub(crate) fn check_for_update() -> Result<Check, String> {
    let installed = installed_tag(&super::app_directory()?)?;
    let available = remote_release_after(Some(&installed))?;
    Ok(Check { installed: installed.build, available })
}

pub(crate) struct StagedRuntime {
    directory: PathBuf,
    created_files: Vec<PathBuf>,
    lease: Option<File>,
    active: bool,
}
impl StagedRuntime {
    pub(crate) fn server(&self) -> PathBuf { self.directory.join("llama-server.exe") }
}
impl Drop for StagedRuntime {
    fn drop(&mut self) {
        self.lease.take();
        if !self.active {
            for file in &self.created_files { let _ = fs::remove_file(file); }
            let _ = fs::remove_dir(&self.directory);
        }
    }
}

fn extract_runtime(stage: &mut StagedRuntime, file: File, base: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(file).map_err(|error| format!("更新ファイルを開けない: {error}"))?;
    if archive.len() > 500 { return Err("更新ファイルの項目が多すぎる。".into()); }
    let mut total = 0u64;
    let mut names = HashSet::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| format!("更新ファイルを読めない: {error}"))?;
        if entry.is_dir() { continue; }
        let name = entry.name().map_err(|error| format!("更新ファイルの名前が不正である: {error}"))?.into_owned();
        let wanted = name == "llama-server.exe" || name.ends_with(".dll") || name == "LICENSE-LLVM-OpenMP";
        if !wanted { continue; }
        if !safe_archive_name(&name) || entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
            return Err("更新ファイルに不正なパスがある。".into());
        }
        if !names.insert(name.to_ascii_lowercase()) { return Err("更新ファイルに重複した名前がある。".into()); }
        let size = entry.size();
        total = total.checked_add(size).ok_or("更新ファイルのサイズが不正である。")?;
        if size > MAX_FILE || total > MAX_EXTRACTED { return Err("更新ファイルのサイズが大きすぎる。".into()); }
        let target = stage.directory.join(&name);
        let mut output = OpenOptions::new().write(true).create_new(true).open(&target)
            .map_err(|error| format!("更新ファイルを保存できない: {error}"))?;
        stage.created_files.push(target);
        let copied = std::io::copy(&mut entry.by_ref().take(size.saturating_add(1)), &mut output)
            .map_err(|error| format!("更新ファイルを展開できない: {error}"))?;
        if copied != size { return Err("更新ファイルの展開サイズが一致しない。".into()); }
        drop(output);
        if name == "llama-server.exe" {
            // Do not let another window's cleanup remove an in-progress update.
            stage.lease = Some(protect_runtime(&stage.directory.join(&name))?);
        }
    }
    for required in ["llama-server.exe", "llama-server-impl.dll", "llama-common.dll", "llama.dll", "ggml.dll", "ggml-base.dll", "ggml-vulkan.dll", "mtmd.dll"] {
        if !stage.directory.join(required).is_file() { return Err(format!("更新ファイルに必要な項目がない: {required}")); }
    }
    // The original runtime may have been removed after a successful update.
    let source = selected_runtime(base).join("LICENSE-llama.cpp");
    let target = stage.directory.join("LICENSE-llama.cpp");
    let mut output = OpenOptions::new().write(true).create_new(true).open(&target)
        .map_err(|error| format!("ライセンスを保存できない: {error}"))?;
    stage.created_files.push(target);
    match File::open(source) {
        Ok(mut input) => { std::io::copy(&mut input, &mut output).map_err(|error| format!("ライセンスを保存できない: {error}"))?; }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            output.write_all(LLAMA_LICENSE).map_err(|error| format!("ライセンスを保存できない: {error}"))?;
        }
        Err(error) => return Err(format!("ライセンスを読み取れない: {error}")),
    }
    Ok(())
}

pub(crate) fn stage_update(release: &Release, status: &impl Fn(String)) -> Result<StagedRuntime, String> {
    let base = super::app_directory()?;
    let nonce = super::process_guard::secret()?;
    let directory = base.join(format!("runtime-vulkan-{}-{}", release.tag().tag(), &nonce[..16]));
    fs::create_dir(&directory).map_err(|error| format!("更新用フォルダを作成できない: {error}"))?;
    let mut stage = StagedRuntime { directory, created_files: Vec::new(), lease: None, active: false };
    let archive_path = stage.directory.join(".download.zip");
    let mut archive = OpenOptions::new().read(true).write(true).create_new(true).open(&archive_path)
        .map_err(|error| format!("更新ファイルを保存できない: {error}"))?;
    stage.created_files.push(archive_path.clone());
    // The engine archive is much larger than release metadata and may take
    // longer to download on a slower connection.
    let client = network_client(Duration::from_secs(30 * 60))?;
    let tag = release.tag().tag();
    let url = format!("https://github.com/ggml-org/llama.cpp/releases/download/{tag}/llama-{tag}-bin-win-vulkan-x64.zip");
    status(format!("llama.cpp b{} をダウンロード中…", release.build));
    let mut response = client.get(url).send().map_err(|error| format!("エンジンを取得できない: {}", error.without_url()))?;
    if !response.status().is_success() { return Err(format!("エンジンを取得できない（HTTP {}）。", response.status())); }
    let expected_length = response.content_length();
    if expected_length.is_some_and(|length| length > MAX_ARCHIVE) { return Err("更新ファイルが大きすぎる。".into()); }
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut received = 0u64;
    let mut last = Instant::now();
    loop {
        let count = response.read(&mut buffer).map_err(|error| format!("エンジンの取得が中断した: {error}"))?;
        if count == 0 { break; }
        received = received.checked_add(count as u64).ok_or("更新ファイルのサイズが不正である。")?;
        if received > MAX_ARCHIVE { return Err("更新ファイルが大きすぎる。".into()); }
        archive.write_all(&buffer[..count]).map_err(|error| format!("更新ファイルを保存できない: {error}"))?;
        digest.update(&buffer[..count]);
        if last.elapsed() >= Duration::from_secs(1) {
            let progress = expected_length.filter(|length| *length > 0).map(|length| format!(" {:.1}%", received as f64 * 100.0 / length as f64)).unwrap_or_default();
            status(format!("llama.cpp b{} をダウンロード中…{progress}", release.build));
            last = Instant::now();
        }
    }
    if expected_length.is_some_and(|length| length != received) { return Err("更新ファイルのサイズが一致しない。".into()); }
    if format!("{:x}", digest.finalize()) != release.sha256 { return Err("更新ファイルの SHA-256 が一致しない。".into()); }
    archive.flush().map_err(|error| format!("更新ファイルを保存できない: {error}"))?;
    archive.seek(SeekFrom::Start(0)).map_err(|error| format!("更新ファイルを読み取れない: {error}"))?;
    status("更新ファイルを検証・展開中…".into());
    extract_runtime(&mut stage, archive, &base)?;
    fs::remove_file(&archive_path).map_err(|error| format!("更新用の一時ファイルを整理できない: {error}"))?;
    validate_engine_identity(&stage.server(), &release.tag())?;
    write_manifest(&mut stage)?;
    Ok(stage)
}

#[cfg(windows)]
fn replace_pointer(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" { fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32; }
    let from: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0x1 | 0x8) } == 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

#[cfg(not(windows))]
fn replace_pointer(source: &Path, target: &Path) -> std::io::Result<()> { fs::rename(source, target) }

fn select_directory(directory: &Path, pointer: &str) -> Result<(), String> {
    let base = directory.parent().ok_or("更新先のフォルダを確認できない。")?;
    let name = directory.file_name().and_then(|name| name.to_str()).ok_or("更新先の名前を確認できない。")?;
    if !runtime_name_is_valid(name) { return Err("更新先の名前が不正である。".into()); }
    let temporary = base.join(format!(".reply-llama-selection-{}.tmp", &super::process_guard::secret()?[..16]));
    let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary)
        .map_err(|error| format!("更新設定を保存できない: {error}"))?;
    let result: Result<(), String> = (|| {
        writeln!(file, "{name}").and_then(|_| file.sync_all()).map_err(|error| format!("更新設定を保存できない: {error}"))?;
        drop(file);
        replace_pointer(&temporary, &base.join(pointer)).map_err(|error| format!("更新設定を切り替えられない: {error}"))?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(&temporary); }
    result
}

pub(crate) fn activate(stage: &mut StagedRuntime) -> Result<(), String> {
    select_directory(&stage.directory, POINTER_NAME)?;
    stage.active = true;
    Ok(())
}

pub(crate) fn defer(stage: &mut StagedRuntime) -> Result<(), String> {
    select_directory(&stage.directory, PENDING_POINTER_NAME)?;
    stage.active = true;
    Ok(())
}

pub(crate) fn discard_pending(base: &Path, pending: &Path) -> Result<(), String> {
    if pending_runtime(base).as_deref() == Some(pending) {
        fs::remove_file(base.join(PENDING_POINTER_NAME)).map_err(|error| format!("更新待ちの設定を整理できない: {error}"))?;
    }
    Ok(())
}

pub(crate) fn require_active_validation(base: &Path) -> Result<(), String> {
    let active = pointed_runtime(base, POINTER_NAME).ok_or("使用中のエンジンを確認できない。")?;
    select_directory(&active, PENDING_POINTER_NAME)
}

pub(crate) fn activate_pending(base: &Path, pending: &Path) -> Result<(), String> {
    if pending_runtime(base).as_deref() != Some(pending) { return Err("更新待ちのエンジンを確認できない。".into()); }
    select_directory(pending, POINTER_NAME)?;
    // The selected runtime is already active; retrying a stale pending marker is safe.
    let _ = discard_pending(base, pending);
    Ok(())
}

fn legacy_runtime_file(name: &str) -> bool {
    const FILES: &[&str] = &[
        "llama-server.exe", "llama.dll", "llama-common.dll", "llama-server-impl.dll",
        "llama-batched-bench-impl.dll", "llama-bench-impl.dll", "llama-cli-impl.dll",
        "llama-completion-impl.dll", "llama-fit-params-impl.dll", "llama-perplexity-impl.dll",
        "llama-quantize-impl.dll", "ggml.dll", "ggml-base.dll", "ggml-vulkan.dll",
        "ggml-rpc.dll", "libomp.dll", "mtmd.dll", "LICENSE-llama.cpp", "LICENSE-LLVM-OpenMP",
    ];
    const CPU: &[&str] = &[
        "alderlake", "cannonlake", "cascadelake", "cooperlake", "haswell", "icelake",
        "ivybridge", "piledriver", "sandybridge", "sapphirerapids", "skylakex", "sse42", "x64", "zen4",
    ];
    FILES.contains(&name) || name.strip_prefix("ggml-cpu-").and_then(|value| value.strip_suffix(".dll"))
        .is_some_and(|variant| CPU.contains(&variant))
}

fn plain_file(meta: &fs::Metadata) -> bool {
    if !meta.is_file() || meta.file_type().is_symlink() { return false; }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Do not remove links or files whose read-only flag may prevent deletion.
        if meta.file_attributes() & (0x400 | 0x1) != 0 { return false; }
    }
    true
}

fn plain_directory(meta: &fs::Metadata) -> bool {
    if !meta.is_dir() || meta.file_type().is_symlink() { return false; }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 { return false; }
    }
    true
}

fn runtime_files(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let name = entry.file_name().into_string().map_err(|_| "名前を確認できない。")?;
        if !safe_archive_name(&name) || !plain_file(&fs::symlink_metadata(&path).map_err(|error| error.to_string())?) {
            return Err("エンジン以外の項目がある。".into());
        }
        files.push(path);
    }
    let server = directory.join("llama-server.exe");
    let vulkan = directory.join("ggml-vulkan.dll");
    if !files.contains(&server) || !files.contains(&vulkan) { return Err("エンジンの構成を確認できない。".into()); }
    let manifest = directory.join(MANIFEST_NAME);
    if files.contains(&manifest) {
        let file = File::open(&manifest).map_err(|error| error.to_string())?;
        if file.metadata().map_err(|error| error.to_string())?.len() > 128 * 1024 { return Err("エンジンの記録が大きすぎる。".into()); }
        let value: serde_json::Value = serde_json::from_reader(file).map_err(|error| error.to_string())?;
        let hashes = value.as_object().ok_or("エンジンの記録が不正である。")?;
        if hashes.len() + 1 != files.len() { return Err("エンジン以外の項目がある。".into()); }
        for path in files.iter().filter(|path| **path != manifest) {
            let name = path.file_name().and_then(|value| value.to_str()).ok_or("名前を確認できない。")?;
            let expected = hashes.get(name).and_then(|value| value.as_str()).ok_or("エンジンの記録が不正である。")?;
            if hash_file(path)? != expected { return Err("エンジンのファイルが変更されている。".into()); }
        }
    } else if files.iter().any(|path| !path.file_name().and_then(|value| value.to_str()).is_some_and(legacy_runtime_file)) {
        return Err("エンジン以外の項目がある。".into());
    }
    Ok(files)
}

#[cfg(windows)]
fn exclusive_server(server: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new().read(true).access_mode(0x0001_0000 | 0x80) // DELETE | FILE_READ_ATTRIBUTES
        .share_mode(0).custom_flags(0x0020_0000) // FILE_FLAG_OPEN_REPARSE_POINT
        .open(server)
}

#[cfg(windows)]
fn remove_exclusive_server(file: File, _server: &Path) -> std::io::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    extern "system" { fn SetFileInformationByHandle(handle: *mut c_void, class: i32, info: *const c_void, size: u32) -> i32; }
    let disposition: u8 = 1; // FILE_DISPOSITION_INFO.DeleteFile is BOOLEAN.
    let ok = unsafe { SetFileInformationByHandle(file.as_raw_handle(), 4, &disposition as *const _ as *const c_void, 1) };
    if ok == 0 { return Err(std::io::Error::last_os_error()); }
    drop(file);
    Ok(())
}

#[cfg(not(windows))]
fn exclusive_server(server: &Path) -> std::io::Result<File> { File::open(server) }

#[cfg(not(windows))]
fn remove_exclusive_server(file: File, server: &Path) -> std::io::Result<()> { drop(file); fs::remove_file(server) }

fn remove_runtime(directory: &Path) -> Result<(), String> {
    let mut files = runtime_files(directory)?;
    let server = directory.join("llama-server.exe");
    // Acquire every handle before removing anything. A separately locked DLL
    // must not cause a partially removed runtime.
    files.sort_by_key(|path| u8::from(*path == server));
    let mut locked = Vec::with_capacity(files.len());
    for path in files {
        let handle = exclusive_server(&path).map_err(|error| format!("使用中か削除できない: {error}"))?;
        if !plain_file(&handle.metadata().map_err(|error| error.to_string())?) { return Err("エンジンのファイルが不正である。".into()); }
        locked.push((path, handle));
    }
    for (path, handle) in locked { remove_exclusive_server(handle, &path).map_err(|error| error.to_string())?; }
    fs::remove_dir(directory).map_err(|error| error.to_string())
}

fn build_from_directory(name: &str) -> Option<u32> {
    Some(tag_from_directory(name)?.build)
}

pub(crate) fn cleanup_older_runtimes(base: &Path) -> Result<(), String> {
    cleanup_older_with(base, engine_build)
}

pub(crate) fn cleanup_replaced_runtimes(base: &Path) -> Result<(), String> {
    cleanup_with(base, engine_build, true)
}

fn cleanup_older_with(base: &Path, baseline_build: impl Fn(&Path) -> Result<u32, String>) -> Result<(), String> {
    cleanup_with(base, baseline_build, false)
}

fn cleanup_with(base: &Path, baseline_build: impl Fn(&Path) -> Result<u32, String>, remove_same: bool) -> Result<(), String> {
    let active = pointed_runtime(base, POINTER_NAME).ok_or("使用中のエンジンを確認できない。")?;
    let active_build = active.file_name().and_then(|name| name.to_str()).and_then(build_from_directory)
        .ok_or("使用中のエンジンの版を確認できない。")?;
    let root = base.canonicalize().map_err(|error| error.to_string())?;
    let pending = pending_runtime(base);
    let mut failures = Vec::new();
    for entry in fs::read_dir(base).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue; };
        if name != "runtime-vulkan" && build_from_directory(&name).is_none() { continue; }
        let directory = entry.path();
        if directory == active || pending.as_ref() == Some(&directory) { continue; }
        let valid = fs::symlink_metadata(&directory).ok().is_some_and(|meta| plain_directory(&meta))
            && directory.canonicalize().ok().is_some_and(|actual| actual.parent() == Some(root.as_path()));
        if !valid { failures.push(name); continue; }
        let build = if name == "runtime-vulkan" {
            // Do not execute or modify an unfamiliar baseline folder.
            if runtime_files(&directory).is_err() { failures.push(name); continue; }
            match baseline_build(&directory.join("llama-server.exe")) {
                Ok(build) => build,
                Err(_) => { failures.push(name); continue; }
            }
        } else { build_from_directory(&name).unwrap() };
        if build > active_build || (build == active_build && (!remove_same || !directory.join(MANIFEST_NAME).is_file())) { continue; }
        if pointed_runtime(base, POINTER_NAME).as_deref() != Some(active.as_path()) {
            return Err("使用中のエンジンが変更されたため、古い版の削除を中止した。".into());
        }
        if remove_runtime(&directory).is_err() { failures.push(name); }
    }
    if failures.is_empty() { Ok(()) }
    else { Err(format!("古いエンジンを削除できなかった。使用状況やフォルダの内容を確認してほしい: {}", failures.join("、"))) }
}

#[cfg(test)]
mod tests {
    use super::{activate, activate_pending, cleanup_older_with, cleanup_with, defer, discard_pending, extract_runtime, find_newer, needs_bootstrap_in, parse_asset_digest, parse_build, parse_feed, parse_tag, pending_matches_in, pending_runtime, require_active_validation, runtime_name_is_valid, safe_archive_name, select_directory, selected_runtime, write_manifest, Release, RuntimeTag, StagedRuntime, LLAMA_LICENSE, PENDING_POINTER_NAME, POINTER_NAME};
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir() -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("reply-runtime-test-{}-{timestamp}-{sequence}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        dir
    }

    fn runtime_fixture(base: &std::path::Path, name: &str) -> PathBuf {
        let dir = base.join(name);
        fs::create_dir(&dir).unwrap();
        for (name, bytes) in [("llama-server.exe", b"server".as_slice()), ("ggml-vulkan.dll", b"vulkan"), ("libomp.dll", b"omp"), ("LICENSE-llama.cpp", b"MIT")] {
            fs::write(dir.join(name), bytes).unwrap();
        }
        dir
    }

    fn remove_fixture(dir: &std::path::Path) {
        for entry in fs::read_dir(dir).unwrap() { fs::remove_file(entry.unwrap().path()).unwrap(); }
        fs::remove_dir(dir).unwrap();
    }

    fn tag(build: u32) -> RuntimeTag {
        parse_tag(&format!("b{build}")).unwrap()
    }

    fn feed(tags: &[RuntimeTag]) -> String {
        tags.iter().map(|tag| format!("<entry><link href=\"https://github.com/ggml-org/llama.cpp/releases/tag/{}\"/></entry>", tag.tag())).collect()
    }

    fn asset(tag: &RuntimeTag, hash: &str) -> String {
        let name = tag.tag();
        format!("<li><a href=\"/ggml-org/llama.cpp/releases/download/{name}/llama-{name}-bin-win-vulkan-x64.zip\">asset</a><span>sha256:{hash}</span></li>")
    }

    #[test]
    fn checks_newest_available_windows_vulkan_build() {
        let hash = "a".repeat(64);
        let mut checked = Vec::new();
        let older = tag(12178);
        let available = tag(12292);
        let found = find_newer(Some(&older), &feed(&[available.clone(), tag(12302), tag(12300), available.clone()]), |candidate| {
            checked.push(candidate.build);
            Ok(if candidate == &available { asset(candidate, &hash) } else { "<li>source only</li>".into() })
        }).unwrap().unwrap();
        assert_eq!(found, Release { build: 12292, sha256: hash });
        assert_eq!(checked, vec![12302, 12300, 12292]);
        assert_eq!(parse_feed(&feed(&[tag(9), tag(100), tag(10)])), vec![tag(100), tag(10), tag(9)]);
        let current = tag(12302);
        assert!(find_newer(Some(&current), &feed(&[available, current.clone()]), |_| panic!("no request needed")).unwrap().is_none());
    }

    #[test]
    fn digest_must_belong_to_the_exact_asset() {
        let hash = "b".repeat(64);
        let current = tag(12292);
        assert_eq!(parse_asset_digest(&asset(&current, &hash), &current).unwrap(), Some(hash));
        assert_eq!(parse_asset_digest(&asset(&tag(12291), &"a".repeat(64)), &current).unwrap(), None);
        let wrong = "<li><a href=\"/ggml-org/llama.cpp/releases/download/b12292/llama-b12292-bin-win-vulkan-x64.zip\">asset</a></li><li>sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</li>";
        assert!(parse_asset_digest(wrong, &current).is_err());
    }

    #[test]
    fn validates_runtime_selection_and_version() {
        assert_eq!(parse_build("version: 0.9.0-dev (build 12292, commit abc)"), Some(12292));
        assert_eq!(parse_build("unknown"), None);
        assert!(runtime_name_is_valid("runtime-vulkan-b12292-0123456789abcdef"));
        assert!(!runtime_name_is_valid("runtime-vulkan-b12292-0123456789abcdef/../outside"));
        assert!(!runtime_name_is_valid("runtime-vulkan-b12292-not-a-hex-value!"));
        assert!(!runtime_name_is_valid("runtime-vulkan-foreign-b12292-abcdef0-0123456789abcdef"));
        assert!(!safe_archive_name("../evil.dll"));
        assert!(!safe_archive_name("NUL.dll"));
        assert!(!safe_archive_name("COM1.dll"));
        assert!(safe_archive_name("ggml-vulkan.dll"));
    }

    #[test]
    fn stages_only_safe_runtime_files_and_commits_selection() {
        let base = test_dir();
        let baseline = base.join("runtime-vulkan");
        fs::create_dir(&baseline).unwrap();
        fs::write(baseline.join("LICENSE-llama.cpp"), b"MIT").unwrap();
        let directory = base.join("runtime-vulkan-b12292-0123456789abcdef");
        fs::create_dir(&directory).unwrap();
        let zip_path = base.join("test.zip");
        let mut writer = zip::ZipWriter::new(File::create(&zip_path).unwrap());
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for name in ["llama-server.exe", "llama-server-impl.dll", "llama-common.dll", "llama.dll", "ggml.dll", "ggml-base.dll", "ggml-vulkan.dll", "mtmd.dll", "libomp.dll", "LICENSE-LLVM-OpenMP", "unneeded.exe"] {
            writer.start_file(name, options).unwrap();
            writer.write_all(b"test").unwrap();
        }
        writer.finish().unwrap();
        let mut stage = StagedRuntime { directory: directory.clone(), created_files: Vec::new(), lease: None, active: false };
        extract_runtime(&mut stage, File::open(&zip_path).unwrap(), &base).unwrap();
        assert!(!directory.join("unneeded.exe").exists());
        assert_eq!(selected_runtime(&base), baseline);
        activate(&mut stage).unwrap();
        assert_eq!(selected_runtime(&base), directory);
        let files = stage.created_files.clone();
        drop(stage);
        for file in files { fs::remove_file(file).unwrap(); }
        fs::remove_dir(directory).unwrap();
        fs::remove_file(base.join(POINTER_NAME)).unwrap();
        fs::remove_file(zip_path).unwrap();
        fs::remove_file(baseline.join("LICENSE-llama.cpp")).unwrap();
        fs::remove_dir(baseline).unwrap();
        fs::remove_dir(base).unwrap();
    }

    #[test]
    fn keeps_downloaded_update_pending_until_explicit_activation() {
        let base = test_dir();
        let baseline = base.join("runtime-vulkan");
        fs::create_dir(&baseline).unwrap();
        let directory = base.join("runtime-vulkan-b12292-0123456789abcdef");
        fs::create_dir(&directory).unwrap();
        let server = directory.join("llama-server.exe");
        fs::write(&server, b"candidate").unwrap();
        let mut stage = StagedRuntime { directory: directory.clone(), created_files: vec![server.clone()], lease: None, active: false };
        defer(&mut stage).unwrap();
        drop(stage);
        assert_eq!(selected_runtime(&base), baseline);
        assert_eq!(pending_runtime(&base), Some(directory.clone()));
        let release = Release { build: 12292, sha256: "0".repeat(64) };
        assert!(pending_matches_in(&base, &release));
        let other_build = Release { build: 12293, ..release };
        assert!(!pending_matches_in(&base, &other_build));
        discard_pending(&base, &base.join("another-runtime")).unwrap();
        assert_eq!(pending_runtime(&base), Some(directory.clone()));
        activate_pending(&base, &directory).unwrap();
        assert_eq!(selected_runtime(&base), directory);
        assert_eq!(pending_runtime(&base), None);
        assert!(!base.join(PENDING_POINTER_NAME).exists());
        fs::remove_file(server).unwrap();
        fs::remove_dir(directory).unwrap();
        fs::remove_file(base.join(POINTER_NAME)).unwrap();
        fs::remove_dir(baseline).unwrap();
        fs::remove_dir(base).unwrap();
    }

    #[test]
    fn cleanup_removes_only_old_runtimes_after_switching() {
        let base = test_dir();
        let baseline = runtime_fixture(&base, "runtime-vulkan");
        let old = runtime_fixture(&base, "runtime-vulkan-b90-0123456789abcdef");
        let pending = runtime_fixture(&base, "runtime-vulkan-b95-0123456789abcdef");
        let active = runtime_fixture(&base, "runtime-vulkan-b100-0123456789abcdef");
        let newer = runtime_fixture(&base, "runtime-vulkan-b101-0123456789abcdef");
        let unrelated = base.join("personal.txt");
        fs::write(&unrelated, b"keep").unwrap();
        assert!(cleanup_older_with(&base, |_| Ok(80)).is_err());
        assert!(baseline.exists() && old.exists());
        select_directory(&active, POINTER_NAME).unwrap();
        select_directory(&pending, PENDING_POINTER_NAME).unwrap();
        cleanup_older_with(&base, |_| Ok(80)).unwrap();
        assert!(!baseline.exists() && !old.exists());
        assert!(active.exists() && pending.exists() && newer.exists() && unrelated.exists());
        assert_eq!(selected_runtime(&base), active);
        fs::remove_file(base.join(POINTER_NAME)).unwrap();
        fs::remove_file(base.join(PENDING_POINTER_NAME)).unwrap();
        fs::remove_file(unrelated).unwrap();
        for directory in [&active, &pending, &newer] { remove_fixture(directory); }
        fs::remove_dir(base).unwrap();
    }

    #[test]
    fn cleanup_preserves_unknown_or_modified_files() {
        let base = test_dir();
        let active = runtime_fixture(&base, "runtime-vulkan-b100-0123456789abcdef");
        let old = runtime_fixture(&base, "runtime-vulkan-b90-0123456789abcdef");
        select_directory(&active, POINTER_NAME).unwrap();
        let note = old.join("personal.txt");
        fs::write(&note, b"keep").unwrap();
        assert!(cleanup_older_with(&base, |_| Ok(80)).is_err());
        assert!(old.join("llama-server.exe").exists() && note.exists());
        fs::remove_file(&note).unwrap();
        let created_files = fs::read_dir(&old).unwrap().map(|entry| entry.unwrap().path()).collect();
        let mut stage = StagedRuntime { directory: old.clone(), created_files, lease: None, active: false };
        write_manifest(&mut stage).unwrap();
        stage.active = true;
        drop(stage);
        fs::write(old.join("libomp.dll"), b"modified").unwrap();
        assert!(cleanup_older_with(&base, |_| Ok(80)).is_err());
        assert!(old.join("llama-server.exe").exists() && old.join("libomp.dll").exists());
        fs::write(old.join("libomp.dll"), b"omp").unwrap();
        cleanup_older_with(&base, |_| Ok(80)).unwrap();
        assert!(!old.exists());
        fs::remove_file(base.join(POINTER_NAME)).unwrap();
        remove_fixture(&active);
        fs::remove_dir(base).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_leaves_a_runtime_used_by_another_session() {
        let base = test_dir();
        let active = runtime_fixture(&base, "runtime-vulkan-b100-0123456789abcdef");
        let old = runtime_fixture(&base, "runtime-vulkan-b90-0123456789abcdef");
        select_directory(&active, POINTER_NAME).unwrap();
        let lease = super::protect_runtime(&old.join("llama-server.exe")).unwrap();
        assert!(cleanup_older_with(&base, |_| Ok(80)).is_err());
        assert!(old.join("llama-server.exe").exists() && old.join("ggml-vulkan.dll").exists());
        drop(lease);
        cleanup_older_with(&base, |_| Ok(80)).unwrap();
        assert!(!old.exists());
        fs::remove_file(base.join(POINTER_NAME)).unwrap();
        remove_fixture(&active);
        fs::remove_dir(base).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_keeps_all_files_when_a_dll_is_in_use() {
        let base = test_dir();
        let active = runtime_fixture(&base, "runtime-vulkan-b100-0123456789abcdef");
        let old = runtime_fixture(&base, "runtime-vulkan-b90-0123456789abcdef");
        select_directory(&active, POINTER_NAME).unwrap();
        let lease = super::protect_runtime(&old.join("libomp.dll")).unwrap();
        assert!(cleanup_older_with(&base, |_| Ok(80)).is_err());
        for name in ["llama-server.exe", "ggml-vulkan.dll", "libomp.dll", "LICENSE-llama.cpp"] {
            assert!(old.join(name).exists(), "{name} was unexpectedly removed");
        }
        drop(lease);
        cleanup_older_with(&base, |_| Ok(80)).unwrap();
        assert!(!old.exists());
        fs::remove_file(base.join(POINTER_NAME)).unwrap();
        remove_fixture(&active);
        fs::remove_dir(base).unwrap();
    }

    #[test]
    fn next_update_copies_license_from_active_runtime_after_baseline_removal() {
        let base = test_dir();
        let active = runtime_fixture(&base, "runtime-vulkan-b100-0123456789abcdef");
        select_directory(&active, POINTER_NAME).unwrap();
        let directory = base.join("runtime-vulkan-b101-0123456789abcdef");
        fs::create_dir(&directory).unwrap();
        let zip_path = base.join("test.zip");
        let mut writer = zip::ZipWriter::new(File::create(&zip_path).unwrap());
        for name in ["llama-server.exe", "llama-server-impl.dll", "llama-common.dll", "llama.dll", "ggml.dll", "ggml-base.dll", "ggml-vulkan.dll", "mtmd.dll", "libomp.dll", "LICENSE-LLVM-OpenMP"] {
            writer.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(b"test").unwrap();
        }
        writer.finish().unwrap();
        let mut stage = StagedRuntime { directory: directory.clone(), created_files: Vec::new(), lease: None, active: false };
        extract_runtime(&mut stage, File::open(&zip_path).unwrap(), &base).unwrap();
        assert_eq!(fs::read(directory.join("LICENSE-llama.cpp")).unwrap(), b"MIT");
        drop(stage);
        assert!(!directory.exists());
        fs::remove_file(zip_path).unwrap();
        fs::remove_file(base.join(POINTER_NAME)).unwrap();
        remove_fixture(&active);
        fs::remove_dir(base).unwrap();
    }

    #[test]
    fn missing_pointer_requires_bootstrap_without_altering_existing_runtimes() {
        let base = test_dir();
        let runtime = runtime_fixture(&base, "runtime-vulkan-b100-0123456789abcdef");
        assert!(needs_bootstrap_in(&base).unwrap());
        select_directory(&runtime, POINTER_NAME).unwrap();
        assert!(!needs_bootstrap_in(&base).unwrap());
        fs::remove_file(base.join(POINTER_NAME)).unwrap();
        assert!(needs_bootstrap_in(&base).unwrap());
        assert!(runtime.join("llama-server.exe").exists());
        fs::write(base.join(POINTER_NAME), b"../invalid").unwrap();
        assert!(needs_bootstrap_in(&base).is_err());
        fs::remove_file(base.join(POINTER_NAME)).unwrap();
        remove_fixture(&runtime);
        fs::remove_dir(base).unwrap();
    }

    #[test]
    fn unrelated_installation_does_not_prevent_bootstrap() {
        let base = test_dir();
        let legacy = runtime_fixture(&base, "runtime-vulkan-foreign-b12317-abcdef0-0123456789abcdef");
        let pointer = base.join(".reply-foreign-runtime");
        fs::write(&pointer, b"runtime-vulkan-foreign-b12317-abcdef0-0123456789abcdef\n").unwrap();
        assert!(needs_bootstrap_in(&base).unwrap());
        assert_eq!(selected_runtime(&base), base.join("runtime-vulkan"));
        assert!(legacy.join("llama-server.exe").exists());
        fs::remove_file(pointer).unwrap();
        remove_fixture(&legacy);
        fs::remove_dir(base).unwrap();
    }

    #[test]
    fn failed_update_preserves_a_requirement_to_verify_the_active_runtime() {
        let base = test_dir();
        let active = runtime_fixture(&base, "runtime-vulkan-b100-0123456789abcdef");
        let candidate = runtime_fixture(&base, "runtime-vulkan-b101-0123456789abcdef");
        select_directory(&active, POINTER_NAME).unwrap();
        select_directory(&candidate, PENDING_POINTER_NAME).unwrap();
        require_active_validation(&base).unwrap();
        assert_eq!(pending_runtime(&base), Some(active.clone()));
        assert_eq!(selected_runtime(&base), active);
        assert!(candidate.exists());
        fs::remove_file(base.join(POINTER_NAME)).unwrap();
        fs::remove_file(base.join(PENDING_POINTER_NAME)).unwrap();
        remove_fixture(&active);
        remove_fixture(&candidate);
        fs::remove_dir(base).unwrap();
    }

    #[test]
    fn bootstrap_cleanup_removes_verified_duplicate_but_preserves_untracked_copy() {
        let base = test_dir();
        let active = runtime_fixture(&base, "runtime-vulkan-b100-0123456789abcdef");
        let duplicate = runtime_fixture(&base, "runtime-vulkan-b100-fedcba9876543210");
        let untracked = runtime_fixture(&base, "runtime-vulkan-b100-1111111111111111");
        let created_files = fs::read_dir(&duplicate).unwrap().map(|entry| entry.unwrap().path()).collect();
        let mut stage = StagedRuntime { directory: duplicate.clone(), created_files, lease: None, active: false };
        write_manifest(&mut stage).unwrap();
        stage.active = true;
        drop(stage);
        select_directory(&active, POINTER_NAME).unwrap();
        cleanup_older_with(&base, |_| Ok(80)).unwrap();
        assert!(duplicate.exists());
        cleanup_with(&base, |_| Ok(80), true).unwrap();
        assert!(!duplicate.exists());
        assert!(untracked.exists() && active.exists());
        fs::remove_file(base.join(POINTER_NAME)).unwrap();
        remove_fixture(&active);
        remove_fixture(&untracked);
        fs::remove_dir(base).unwrap();
    }

    #[test]
    fn bootstrap_extracts_without_an_existing_engine_license() {
        let base = test_dir();
        let directory = base.join("runtime-vulkan-b101-0123456789abcdef");
        fs::create_dir(&directory).unwrap();
        let zip_path = base.join("test.zip");
        let mut writer = zip::ZipWriter::new(File::create(&zip_path).unwrap());
        for name in ["llama-server.exe", "llama-server-impl.dll", "llama-common.dll", "llama.dll", "ggml.dll", "ggml-base.dll", "ggml-vulkan.dll", "mtmd.dll"] {
            writer.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(b"test").unwrap();
        }
        writer.finish().unwrap();
        let mut stage = StagedRuntime { directory: directory.clone(), created_files: Vec::new(), lease: None, active: false };
        extract_runtime(&mut stage, File::open(&zip_path).unwrap(), &base).unwrap();
        assert_eq!(fs::read(directory.join("LICENSE-llama.cpp")).unwrap(), LLAMA_LICENSE);
        drop(stage);
        assert!(!directory.exists());
        fs::remove_file(zip_path).unwrap();
        fs::remove_dir(base).unwrap();
    }

    #[test]
    fn rejects_archive_traversal_before_writing_outside_stage() {
        let base = test_dir();
        let directory = base.join("runtime-vulkan-b12292-0123456789abcdef");
        fs::create_dir(&directory).unwrap();
        let zip_path = base.join("test.zip");
        let mut writer = zip::ZipWriter::new(File::create(&zip_path).unwrap());
        writer.start_file("../evil.dll", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(b"test").unwrap();
        writer.finish().unwrap();
        let mut stage = StagedRuntime { directory: directory.clone(), created_files: Vec::new(), lease: None, active: false };
        assert!(extract_runtime(&mut stage, File::open(&zip_path).unwrap(), &base).is_err());
        assert!(!base.join("evil.dll").exists());
        drop(stage);
        fs::remove_file(zip_path).unwrap();
        fs::remove_dir(base).unwrap();
    }
}

use reqwest::blocking::Client;
use reqwest::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn partial_path(model: &Path) -> Result<PathBuf, String> {
    let name = model.file_name().ok_or("モデルのファイル名を取得できない。")?.to_string_lossy();
    Ok(model.with_file_name(format!(".reply-{name}.part")))
}

fn parse_content_range(text: &str, expected_start: u64, expected_total: u64) -> Option<u64> {
    let values = text.strip_prefix("bytes ")?;
    let (span, total) = values.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let (start, end, total) = (start.parse::<u64>().ok()?, end.parse::<u64>().ok()?, total.parse::<u64>().ok()?);
    if start != expected_start || total != expected_total || end < start || end >= total { return None; }
    end.checked_sub(start)?.checked_add(1)
}

fn open_partial(path: &Path) -> Result<File, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() => return Err(format!("ダウンロード用の一時ファイルが通常のファイルではない: {}", path.display())),
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(format!("一時ファイルを確認できない: {error}")),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0); // only one process can write or finalize the partial download
    }
    options.open(path).map_err(|error| format!("一時ファイルを開けない。別のアプリがダウンロード中か、保存先を確認してほしい: {error}"))
}

// Unlike std::fs::rename on Windows, MoveFileW fails if the target exists.
// A model placed by the user or another instance must never be overwritten.
#[cfg(windows)]
fn publish_without_replacing(partial: &Path, model: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" { fn MoveFileW(existing: *const u16, new_name: *const u16) -> i32; }
    let from: Vec<u16> = partial.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = model.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileW(from.as_ptr(), to.as_ptr()) } == 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

#[cfg(not(windows))]
fn publish_without_replacing(partial: &Path, model: &Path) -> std::io::Result<()> {
    fs::hard_link(partial, model)?;
    fs::remove_file(partial)
}

pub(super) fn download_model(model: &Path, url: &str, expected_size: u64, expected_sha256: &str, status: &impl Fn(String)) -> Result<(), String> {
    let client = Client::builder().https_only(true).connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(12 * 60 * 60))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(concat!("ReplyJP/", env!("CARGO_PKG_VERSION")))
        .build().map_err(|error| format!("ダウンロードの準備に失敗した: {error}"))?;
    download_with_client(&client, model, url, expected_size, expected_sha256, status)
}

fn download_with_client(client: &Client, model: &Path, url: &str, expected_size: u64, expected_sha256: &str, status: &impl Fn(String)) -> Result<(), String> {
    if expected_size == 0 { return Err("モデルのサイズが不正である。".into()); }
    if fs::symlink_metadata(model).is_ok() {
        return Err(format!("モデルが既に存在するため上書きしない: {}", model.display()));
    }
    let partial = partial_path(model)?;
    let mut file = open_partial(&partial)?;
    let mut offset = file.metadata().map_err(|error| format!("一時ファイルを確認できない: {error}"))?.len();
    if offset > expected_size {
        return Err(format!("一時ファイルのサイズが不正である。内容を確認してほしい: {}", partial.display()));
    }

    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    if offset > 0 {
        status("ダウンロード済みのデータを確認中…".into());
        file.seek(SeekFrom::Start(0)).map_err(|error| format!("一時ファイルを読み取れない: {error}"))?;
        loop {
            let n = file.read(&mut buffer).map_err(|error| format!("一時ファイルを読み取れない: {error}"))?;
            if n == 0 { break; }
            hash.update(&buffer[..n]);
        }
    }

    if offset < expected_size {
        status(format!("モデルをダウンロード中… {:.1}%（約{:.2} GB）", offset as f64 * 100.0 / expected_size as f64, expected_size as f64 / 1_000_000_000.0));
        let mut request = client.get(url).header(ACCEPT_ENCODING, "identity");
        if offset > 0 { request = request.header(RANGE, format!("bytes={offset}-")); }
        let mut response = request.send().map_err(|error| format!("モデルをダウンロードできない: {}", error.without_url()))?;
        let response_status = response.status();
        if !response_status.is_success() {
            return Err(format!("モデルをダウンロードできない（HTTP {response_status}）。時間を置いて再試行してほしい。"));
        }
        if response.headers().get(CONTENT_ENCODING).and_then(|value| value.to_str().ok())
            .is_some_and(|encoding| !encoding.eq_ignore_ascii_case("identity")) {
            return Err("モデルのダウンロード形式が不正である。".into());
        }
        let expected_response = if response_status.as_u16() == 206 {
            let range = response.headers().get(CONTENT_RANGE).and_then(|value| value.to_str().ok())
                .ok_or("再開位置を確認できなかった。")?;
            parse_content_range(range, offset, expected_size).ok_or("ダウンロードの再開位置が一致しない。")?
        } else if response_status.as_u16() == 200 {
            if offset > 0 {
                // Some servers ignore Range. Restart with this full response.
                status("配布元が再開に対応していないため、最初から取得中…".into());
                file.set_len(0).map_err(|error| format!("一時ファイルを更新できない: {error}"))?;
                file.seek(SeekFrom::Start(0)).map_err(|error| format!("一時ファイルを更新できない: {error}"))?;
                offset = 0;
                hash = Sha256::new();
            }
            expected_size
        } else {
            return Err(format!("モデルのダウンロード応答が不正である（HTTP {response_status}）。"));
        };
        if let Some(length) = response.headers().get(CONTENT_LENGTH) {
            let length = length.to_str().ok().and_then(|value| value.parse::<u64>().ok());
            if length != Some(expected_response) { return Err("モデルのダウンロードサイズが一致しない。".into()); }
        }
        let mut last_update = Instant::now();
        loop {
            let n = response.read(&mut buffer).map_err(|error| format!("ダウンロードが中断した。次回は続きを試みる: {error}"))?;
            if n == 0 { break; }
            if offset.saturating_add(n as u64) > expected_size { return Err("受信したモデルが指定サイズを超えた。".into()); }
            file.write_all(&buffer[..n]).map_err(|error| format!("モデルを保存できない。空き容量と保存先を確認してほしい: {error}"))?;
            hash.update(&buffer[..n]);
            offset += n as u64;
            if last_update.elapsed() >= Duration::from_secs(1) {
                status(format!("モデルをダウンロード中… {:.1}%（約{:.2} GB）", offset as f64 * 100.0 / expected_size as f64, expected_size as f64 / 1_000_000_000.0));
                last_update = Instant::now();
            }
        }
        if offset != expected_size { return Err("ダウンロードが途中で終了した。次回は続きを試みる。".into()); }
    }

    status("ダウンロードしたモデルを検証中…".into());
    if format!("{:x}", hash.finalize()) != expected_sha256.to_ascii_lowercase() {
        // This path is reserved for this application's incomplete downloads;
        // the separately named final model is never modified here.
        file.set_len(0).map_err(|error| format!("不正な一時データを破棄できない: {error}"))?;
        return Err("モデルの SHA-256 が一致しない。一時データを破棄した。再実行で取得し直す。".into());
    }
    file.flush().and_then(|_| file.sync_all()).map_err(|error| format!("モデルを保存できない: {error}"))?;
    drop(file);
    publish_without_replacing(&partial, model).map_err(|error| format!("モデルを配置できない。既存ファイルは上書きしない: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{download_with_client, parse_content_range, partial_path, publish_without_replacing};
    use reqwest::blocking::Client;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct TestFiles { model: PathBuf, partial: PathBuf }
    impl TestFiles {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let model = std::env::temp_dir().join(format!("reply-download-test-{}-{unique}-{sequence}.gguf", std::process::id()));
            let partial = partial_path(&model).unwrap();
            Self { model, partial }
        }
    }
    impl Drop for TestFiles {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.model);
            let _ = fs::remove_file(&self.partial);
        }
    }
    fn hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
    fn client() -> Client { Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap() }

    fn server(response_head: &str, body: &[u8]) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let head = response_head.to_owned();
        let body = body.to_vec();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(6);
            let (mut connection, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                    Err(error) => return format!("server did not receive request: {error}"),
                }
            };
            // Windows accepted sockets can inherit the listener's nonblocking mode.
            connection.set_nonblocking(false).unwrap();
            connection.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let n = connection.read(&mut chunk).unwrap();
                if n == 0 { break; }
                request.extend_from_slice(&chunk[..n]);
            }
            let _ = connection.write_all(head.as_bytes());
            let _ = connection.write_all(&body);
            String::from_utf8_lossy(&request).to_ascii_lowercase()
        });
        (url, worker)
    }

    #[test]
    fn downloads_to_temporary_file_and_verifies_before_publishing() {
        let files = TestFiles::new();
        let bytes = b"GGUF-test-data";
        let (url, worker) = server(&format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()), bytes);
        download_with_client(&client(), &files.model, &url, bytes.len() as u64, &hash(bytes), &|_| {}).unwrap();
        assert_eq!(fs::read(&files.model).unwrap(), bytes);
        assert!(!files.partial.exists());
        assert!(!worker.join().unwrap().contains("range:"));
    }

    #[test]
    fn resumes_interrupted_file_only_at_the_confirmed_range() {
        let files = TestFiles::new();
        let bytes = b"GGUF-test-data";
        fs::write(&files.partial, &bytes[..5]).unwrap();
        let (url, worker) = server(&format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 5-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len() - 1, bytes.len(), bytes.len() - 5), &bytes[5..]);
        download_with_client(&client(), &files.model, &url, bytes.len() as u64, &hash(bytes), &|_| {}).unwrap();
        assert_eq!(fs::read(&files.model).unwrap(), bytes);
        assert!(worker.join().unwrap().contains("range: bytes=5-"));
    }

    #[test]
    fn preserves_a_network_interruption_and_resumes_on_the_next_attempt() {
        let files = TestFiles::new();
        let bytes = b"GGUF-test-data";
        let (url, first_worker) = server(&format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()), &bytes[..5]);
        assert!(download_with_client(&client(), &files.model, &url, bytes.len() as u64, &hash(bytes), &|_| {}).is_err());
        first_worker.join().unwrap();
        assert_eq!(fs::read(&files.partial).unwrap(), b"GGUF-");
        assert!(!files.model.exists());
        let (url, second_worker) = server(&format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 5-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len() - 1, bytes.len(), bytes.len() - 5), &bytes[5..]);
        download_with_client(&client(), &files.model, &url, bytes.len() as u64, &hash(bytes), &|_| {}).unwrap();
        assert!(second_worker.join().unwrap().contains("range: bytes=5-"));
        assert_eq!(fs::read(&files.model).unwrap(), bytes);
    }

    #[test]
    fn restarts_if_the_server_does_not_support_ranges() {
        let files = TestFiles::new();
        let bytes = b"GGUF-test-data";
        fs::write(&files.partial, b"GGUF-").unwrap();
        let (url, worker) = server(&format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()), bytes);
        download_with_client(&client(), &files.model, &url, bytes.len() as u64, &hash(bytes), &|_| {}).unwrap();
        assert_eq!(fs::read(&files.model).unwrap(), bytes);
        assert!(worker.join().unwrap().contains("range: bytes=5-"));
    }

    #[test]
    fn rejects_wrong_range_without_changing_the_partial_file() {
        let files = TestFiles::new();
        let bytes = b"GGUF-test-data";
        fs::write(&files.partial, b"GGUF-").unwrap();
        let (url, worker) = server(&format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len() - 1, bytes.len(), bytes.len()), bytes);
        assert!(download_with_client(&client(), &files.model, &url, bytes.len() as u64, &hash(bytes), &|_| {}).is_err());
        assert_eq!(fs::read(&files.partial).unwrap(), b"GGUF-");
        assert!(!files.model.exists());
        worker.join().unwrap();
    }

    #[test]
    fn rejects_corrupted_download_and_keeps_final_path_empty() {
        let files = TestFiles::new();
        let bytes = b"GGUF-test-data";
        let (url, worker) = server(&format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()), bytes);
        assert!(download_with_client(&client(), &files.model, &url, bytes.len() as u64, &hash(b"different"), &|_| {}).is_err());
        assert!(!files.model.exists());
        assert_eq!(fs::metadata(&files.partial).unwrap().len(), 0);
        worker.join().unwrap();
    }

    #[test]
    fn refuses_to_overwrite_an_existing_model_even_at_publication() {
        let files = TestFiles::new();
        fs::write(&files.model, b"user model").unwrap();
        fs::write(&files.partial, b"downloaded").unwrap();
        assert!(download_with_client(&client(), &files.model, "http://127.0.0.1:1/model", 10, &hash(b"downloaded"), &|_| {}).is_err());
        assert!(publish_without_replacing(&files.partial, &files.model).is_err());
        assert_eq!(fs::read(&files.model).unwrap(), b"user model");
        assert_eq!(fs::read(&files.partial).unwrap(), b"downloaded");
    }

    #[test]
    fn validates_range_boundaries() {
        assert_eq!(parse_content_range("bytes 5-13/14", 5, 14), Some(9));
        assert_eq!(parse_content_range("bytes 4-13/14", 5, 14), None);
        assert_eq!(parse_content_range("bytes 5-14/14", 5, 14), None);
        assert_eq!(parse_content_range("bytes 5-13/*", 5, 14), None);
    }
}

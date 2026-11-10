//! Small loopback-only, byte-range-aware server used by the desktop preview.
use anyhow::{bail, Context, Result};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub struct PreviewServer {
    pub url: String,
    stop: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
}

impl Drop for PreviewServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
    }
}

pub fn start(root: PathBuf, port: u16) -> Result<PreviewServer> {
    let root = root.canonicalize().context("Cannot open preview folder")?;
    if !root.is_dir() {
        bail!("Preview path must be a folder");
    }
    let listener = TcpListener::bind(("127.0.0.1", port))
        .context("Cannot bind local preview port; choose another port")?;
    let address = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let active = Arc::new(AtomicUsize::new(0));
    let handle = thread::spawn(move || {
        while !stopping.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    if active.load(Ordering::Relaxed) >= 32 {
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                        let _ = error(&mut stream, 503, "Too many preview requests", false);
                        continue;
                    }
                    active.fetch_add(1, Ordering::Relaxed);
                    let active = active.clone();
                    let root = root.clone();
                    thread::spawn(move || {
                        let _ = handle_request(&mut stream, &root);
                        active.fetch_sub(1, Ordering::Relaxed);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    });
    Ok(PreviewServer {
        url: format!("http://{address}/"),
        stop,
        listener: Some(handle),
    })
}

fn handle_request(stream: &mut TcpStream, root: &Path) -> std::io::Result<()> {
    // Accepted sockets may inherit the listener's nonblocking mode on Windows.
    // A large script/range must wait for socket backpressure instead of ending
    // early with WouldBlock and a truncated Content-Length response.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut header = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        header.extend_from_slice(&chunk[..n]);
        if header.len() > 16_384 {
            return error(stream, 431, "Request headers too large", false);
        }
        if header.windows(4).any(|s| s == b"\r\n\r\n") {
            break;
        }
    }
    let Ok(header) = std::str::from_utf8(&header) else {
        return error(stream, 400, "Invalid request", false);
    };
    let mut lines = header.split("\r\n");
    let first: Vec<_> = lines.next().unwrap_or("").split_whitespace().collect();
    if first.len() != 3 || !matches!(first[2], "HTTP/1.0" | "HTTP/1.1") {
        return error(stream, 400, "Invalid request", false);
    }
    let head = first[0] == "HEAD";
    if !head && first[0] != "GET" {
        return error(stream, 405, "Only GET and HEAD are supported", false);
    }
    let mut range = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("range") {
                if range.is_some() {
                    return error(stream, 400, "Duplicate Range header", head);
                }
                range = Some(value.trim());
            }
        }
    }
    let path = match resolve(root, first[1]) {
        Ok(p) => p,
        Err(_) => return error(stream, 404, "File not found", head),
    };
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(_) => return error(stream, 404, "File not found", head),
    };
    let length = file.metadata()?.len();
    let (status, start, count) = if let Some(range) = range {
        let Some((start, end)) = parse_range(range, length) else {
            write!(stream, "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{length}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
            return Ok(());
        };
        ("206 Partial Content", start, end - start + 1)
    } else {
        ("200 OK", 0, length)
    };
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {}\r\nContent-Length: {count}\r\nAccept-Ranges: bytes\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n", mime(&path))?;
    if range.is_some() {
        write!(
            stream,
            "Content-Range: bytes {start}-{}/{length}\r\n",
            start + count - 1
        )?;
    }
    stream.write_all(b"\r\n")?;
    if !head && count > 0 {
        file.seek(SeekFrom::Start(start))?;
        std::io::copy(&mut file.take(count), stream)?;
    }
    Ok(())
}

fn error(stream: &mut TcpStream, code: u16, message: &str, head: bool) -> std::io::Result<()> {
    let reason = match code {
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    };
    write!(stream, "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", message.len())?;
    if !head {
        stream.write_all(message.as_bytes())?;
    }
    Ok(())
}

fn resolve(root: &Path, target: &str) -> Result<PathBuf> {
    let raw = target.split(['?', '#']).next().unwrap_or("");
    if !raw.starts_with('/') {
        bail!("Expected an absolute URL path");
    }
    let mut decoded = Vec::with_capacity(raw.len());
    let mut chars = raw.bytes();
    while let Some(c) = chars.next() {
        if c == b'%' {
            let hi = (chars.next().context("Invalid URL escape")? as char)
                .to_digit(16)
                .context("Invalid URL escape")?;
            let lo = (chars.next().context("Invalid URL escape")? as char)
                .to_digit(16)
                .context("Invalid URL escape")?;
            decoded.push((hi * 16 + lo) as u8);
        } else {
            decoded.push(c);
        }
    }
    let decoded = String::from_utf8(decoded)?;
    if decoded.contains(['\\', ':', '\0']) {
        bail!("Invalid path");
    }
    let mut path = root.to_path_buf();
    for part in decoded.split('/') {
        if part == "." || part == ".." {
            bail!("Invalid path component");
        }
        if !part.is_empty() {
            path.push(part);
        }
    }
    if path == root {
        path = default_page(root)?;
    }
    let path = path.canonicalize()?;
    if !path.starts_with(root) || !path.is_file() {
        bail!("Path is outside preview root or is not a file");
    }
    Ok(path)
}

fn default_page(root: &Path) -> Result<PathBuf> {
    if root.join("index.html").is_file() {
        return Ok(root.join("index.html"));
    }
    let mut pages = fs::read_dir(root)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("html"))
        })
        .collect::<Vec<_>>();
    pages.sort();
    pages
        .into_iter()
        .next()
        .context("No HTML viewer in this folder")
}

fn parse_range(value: &str, length: u64) -> Option<(u64, u64)> {
    let range = value.strip_prefix("bytes=")?;
    if length == 0 || range.contains(',') {
        return None;
    }
    let (start, end) = range.split_once('-')?;
    if start.is_empty() {
        let count: u64 = end.parse().ok()?;
        return (count > 0).then(|| (length.saturating_sub(count), length - 1));
    }
    let start: u64 = start.parse().ok()?;
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>().ok()?.min(length - 1)
    };
    (start < length && start <= end).then_some((start, end))
}

fn mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript",
        "json" | "map" => "application/json",
        "css" => "text/css",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "obj" | "mtl" | "txt" | "glsl" | "vert" | "frag" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(url: &str, request: &str) -> String {
        let address = url.trim_start_matches("http://").trim_end_matches('/');
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn preview_streams_ranges_and_head() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("octree.bin"), "0123456789").unwrap();
        fs::write(temp.path().join("Viewer.html"), "viewer").unwrap();
        let server = start(temp.path().to_path_buf(), 0).unwrap();
        let response = request(
            &server.url,
            "GET /octree.bin HTTP/1.1\r\nRange: bytes=2-5\r\n\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 206"));
        assert!(response.contains("Content-Range: bytes 2-5/10"));
        assert!(response.ends_with("\r\n\r\n2345"));
        let head = request(&server.url, "HEAD /octree.bin HTTP/1.1\r\n\r\n");
        assert!(head.contains("Content-Length: 10"));
        assert!(head.ends_with("\r\n\r\n"));
        assert!(request(&server.url, "GET / HTTP/1.1\r\n\r\n").ends_with("viewer"));
        assert!(request(
            &server.url,
            "GET /octree.bin HTTP/1.1\r\nRange: bytes=11-\r\n\r\n"
        )
        .starts_with("HTTP/1.1 416"));
    }

    #[test]
    fn large_responses_survive_socket_backpressure() {
        let temp = tempfile::tempdir().unwrap();
        let body = vec![b'x'; 8 * 1024 * 1024];
        fs::write(temp.path().join("large.js"), &body).unwrap();
        let server = start(temp.path().to_path_buf(), 0).unwrap();
        let address = server
            .url
            .trim_start_matches("http://")
            .trim_end_matches('/');
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .write_all(b"GET /large.js HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        // Let the sender fill its buffer before the receiver drains it.
        thread::sleep(Duration::from_millis(150));
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let header_end = response
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 4;
        assert_eq!(&response[header_end..], body.as_slice());
        assert!(String::from_utf8_lossy(&response[..header_end])
            .contains(&format!("Content-Length: {}", body.len())));
    }

    #[test]
    fn rejects_encoded_traversal_and_windows_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for path in [
            "/../secret",
            "/%2e%2e/secret",
            "/C:%5csecret",
            "/%00",
            "/%xx",
            "https://example.com",
        ] {
            assert!(resolve(&root, path).is_err(), "{path}");
        }
        fs::write(root.join("with space.txt"), "ok").unwrap();
        assert!(resolve(&root, "/with%20space.txt?v=1").is_ok());
    }

    #[test]
    fn range_edge_cases() {
        assert_eq!(parse_range("bytes=-3", 10), Some((7, 9)));
        assert_eq!(parse_range("bytes=8-100", 10), Some((8, 9)));
        assert_eq!(parse_range("bytes=0-", 0), None);
        assert_eq!(parse_range("bytes=-0", 10), None);
        assert_eq!(parse_range("bytes=5-3", 10), None);
        assert_eq!(parse_range("bytes=0-1,3-4", 10), None);
    }
}

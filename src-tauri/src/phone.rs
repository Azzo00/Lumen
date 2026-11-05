//! Shelf → phone over the local Wi-Fi. "Telefono" starts a small HTTP server bound
//! to the PC's LAN address (never the VPN or virtual adapters), with a random
//! 128-bit token in every URL, and shows it as a QR code. The phone opens the page
//! with its camera: it can download the files on the shelf and upload files, which
//! are saved in Download\Dal telefono and put on the shelf. The share stops after
//! 10 minutes or when it is closed.

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const LIFETIME: Duration = Duration::from_secs(10 * 60);
const PAGE: &str = include_str!("phone.html");

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ShareInfo {
    url: String,
    qr_svg: String,
    /// unix ms
    expires_ms: i64,
    network: String,
}

struct Share {
    info: ShareInfo,
    stop: Arc<AtomicBool>,
}

static SHARE: Mutex<Option<Share>> = Mutex::new(None);

/// What a running server needs: the secret token, where uploads go, and what to do
/// with a received file.
struct Ctx {
    token: String,
    dir: PathBuf,
    on_received: Box<dyn Fn(&Path, u64) + Send + Sync>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Received {
    name: String,
    size: u64,
}

/// Starts (or returns the running) share.
#[tauri::command]
pub async fn phone_share_start(app: AppHandle) -> Result<ShareInfo, String> {
    let mut guard = SHARE.lock().map_err(|e| e.to_string())?;
    if let Some(s) = guard.as_ref() {
        if !s.stop.load(Ordering::SeqCst) {
            return Ok(s.info.clone());
        }
    }
    let dir = received_dir().ok_or("cartella Download non trovata")?;
    let (ip, network) = lan_address().ok_or(
        "Nessuna rete Wi-Fi o Ethernet trovata: collega il PC alla stessa rete del telefono.".to_string(),
    )?;
    let server = Server::http((ip, 0)).map_err(|e| format!("Impossibile avviare la condivisione ({e})"))?;
    let port = server.server_addr().to_ip().map(|a| a.port()).ok_or("porta non disponibile")?;
    let token = new_token()?;
    let url = format!("http://{ip}:{port}/{token}");
    let qr_svg = qrcode::QrCode::new(url.as_bytes())
        .map_err(|e| e.to_string())?
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(150, 150)
        .quiet_zone(true)
        .dark_color(qrcode::render::svg::Color("#000000"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    let expires_ms = chrono::Utc::now().timestamp_millis() + LIFETIME.as_millis() as i64;
    let info = ShareInfo { url, qr_svg, expires_ms, network };
    let stop = Arc::new(AtomicBool::new(false));
    *guard = Some(Share { info: info.clone(), stop: stop.clone() });

    let app2 = app.clone();
    let ctx = Arc::new(Ctx {
        token,
        dir,
        on_received: Box::new(move |path: &Path, size: u64| {
            crate::shelf::add_paths(&app2, &[path.to_path_buf()]);
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let _ = app2.emit("phone-received", Received { name, size });
        }),
    });
    std::thread::spawn(move || {
        serve(server, ctx, stop, LIFETIME);
        let _ = app.emit("phone-share-ended", ());
    });
    Ok(info)
}

/// Answers requests until stopped or expired.
fn serve(server: Server, ctx: Arc<Ctx>, stop: Arc<AtomicBool>, lifetime: Duration) {
    let started = Instant::now();
    while !stop.load(Ordering::SeqCst) && started.elapsed() < lifetime {
        match server.recv_timeout(Duration::from_millis(300)) {
            Ok(Some(req)) => {
                let ctx = ctx.clone();
                std::thread::spawn(move || handle(req, &ctx));
            }
            Ok(None) => {}
            Err(_) => break,
        }
    }
    stop.store(true, Ordering::SeqCst);
}

#[tauri::command]
pub fn phone_share_stop() {
    if let Ok(g) = SHARE.lock() {
        if let Some(s) = g.as_ref() {
            s.stop.store(true, Ordering::SeqCst);
        }
    }
}

fn new_token() -> Result<String, String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| e.to_string())?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn is_private(ip: &Ipv4Addr) -> bool {
    ip.is_private() && !ip.is_loopback() && !ip.is_link_local()
}

/// The PC's address on the home/office network: a private IPv4 on a real Wi-Fi or
/// Ethernet adapter. VPN, WSL, Hyper-V and other virtual adapters are skipped.
fn lan_address() -> Option<(Ipv4Addr, String)> {
    let skip = ["vethernet", "loopback", "fortinet", "vpn", "virtual", "hyper-v", "wsl", "vmware", "virtualbox", "bluetooth", "tap", "tun", "docker"];
    let list = local_ip_address::list_afinet_netifas().ok()?;
    let mut cands: Vec<(Ipv4Addr, String)> = list
        .into_iter()
        .filter_map(|(name, ip)| match ip {
            IpAddr::V4(v4) if is_private(&v4) => Some((v4, name)),
            _ => None,
        })
        .filter(|(_, name)| {
            let n = name.to_lowercase();
            !skip.iter().any(|k| n.contains(k))
        })
        .collect();
    // Wi-Fi first (the phone is on Wi-Fi), then Ethernet, then the rest
    let rank = |n: &str| {
        let n = n.to_lowercase();
        if n.contains("wi-fi") || n.contains("wifi") || n.contains("wlan") || n.contains("wireless") {
            0
        } else if n.contains("ethernet") {
            1
        } else {
            2
        }
    };
    cands.sort_by_key(|(_, n)| rank(n));
    cands.into_iter().next()
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn respond_text(req: Request, code: u16, ctype: &str, body: String) {
    let _ = req.respond(
        Response::from_string(body)
            .with_status_code(StatusCode(code))
            .with_header(header("Content-Type", ctype))
            .with_header(header("Cache-Control", "no-store")),
    );
}

/// Routes, all under /<token>: page, list.json, f/<id> (download), up?name= (PUT upload).
fn handle(mut req: Request, ctx: &Ctx) {
    let token = ctx.token.as_str();
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let Some(rest) = path.strip_prefix('/').and_then(|p| p.strip_prefix(token)) else {
        return respond_text(req, 404, "text/plain", "Link scaduto o non valido".into());
    };
    match (req.method().clone(), rest) {
        (Method::Get, "" | "/") => respond_text(req, 200, "text/html; charset=utf-8", PAGE.to_string()),
        (Method::Get, "/list.json") => {
            let files: Vec<serde_json::Value> = crate::shelf::items()
                .into_iter()
                .filter(|i| !i.is_dir)
                .map(|i| serde_json::json!({ "id": i.id, "name": i.name, "size": i.size }))
                .collect();
            respond_text(req, 200, "application/json", serde_json::Value::Array(files).to_string())
        }
        (Method::Get, r) if r.starts_with("/f/") => {
            let item = r[3..].parse::<u32>().ok().and_then(crate::shelf::item).filter(|i| !i.is_dir);
            let Some(item) = item else { return respond_text(req, 404, "text/plain", "File non trovato".into()) };
            let Ok(file) = std::fs::File::open(&item.path) else {
                return respond_text(req, 404, "text/plain", "File non leggibile".into());
            };
            let disp = format!("attachment; filename=\"{}\"; filename*=UTF-8''{}", ascii_name(&item.name), pct(&item.name));
            let resp = Response::new(
                StatusCode(200),
                vec![header("Content-Type", "application/octet-stream"), header("Content-Disposition", &disp)],
                file,
                Some(item.size as usize),
                None,
            );
            let _ = req.respond(resp);
        }
        (Method::Put, "/up") => {
            let name = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("name="))
                .map(unpct)
                .map(|n| safe_name(&n))
                .unwrap_or_else(|| "file".into());
            match save_upload(req.as_reader(), &ctx.dir, &name) {
                Ok((path, size)) => {
                    (ctx.on_received)(&path, size);
                    respond_text(req, 200, "application/json", "{\"ok\":true}".into())
                }
                Err(e) => respond_text(req, 500, "text/plain", e),
            }
        }
        _ => respond_text(req, 404, "text/plain", "Non trovato".into()),
    }
}

fn received_dir() -> Option<PathBuf> {
    crate::downloads::downloads_dir().map(|d| d.join("Dal telefono"))
}

fn save_upload(body: &mut dyn Read, dir: &Path, name: &str) -> Result<(PathBuf, u64), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = unique(dir, name);
    let mut f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    match std::io::copy(body, &mut f) {
        Ok(n) => Ok((path, n)),
        Err(e) => {
            drop(f);
            let _ = std::fs::remove_file(&path);
            Err(format!("caricamento interrotto ({e})"))
        }
    }
}

/// name.ext, name (2).ext, ...
fn unique(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (2..).map(|i| dir.join(format!("{stem} ({i}){ext}"))).find(|p| !p.exists()).unwrap()
}

/// File name from the phone: no folders, no characters Windows refuses.
fn safe_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() { "file".into() } else { cleaned.chars().take(150).collect() }
}

fn ascii_name(n: &str) -> String {
    n.chars().map(|c| if c.is_ascii() && c != '"' && !c.is_control() { c } else { '_' }).collect()
}

fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

fn unpct(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(safe_name("../../Windows/evil.exe"), "evil.exe");
        assert_eq!(safe_name("C:\\x\\a:b?.jpg"), "a_b_.jpg");
        assert_eq!(safe_name("..."), "file");
        assert_eq!(unpct("foto%20mare%C3%A0.jpg"), "foto mareà.jpg");
        assert_eq!(unpct("a%2"), "a%2");
        assert_eq!(pct("à b.pdf"), "%C3%A0%20b.pdf");
    }

    #[test]
    fn unique_names() {
        let dir = std::env::temp_dir().join(format!("lumen-phone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        std::fs::write(dir.join("a (2).txt"), "x").unwrap();
        assert_eq!(unique(&dir, "a.txt").file_name().unwrap(), "a (3).txt");
        assert_eq!(unique(&dir, "b").file_name().unwrap(), "b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Raw HTTP/1.0 request → (status, body bytes).
    fn http(port: u16, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
        use std::io::Write;
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "{method} {path} HTTP/1.0\r\nHost: x\r\nContent-Length: {}\r\n\r\n", body.len()).unwrap();
        s.write_all(body).unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        let split = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&out[..split]).to_string();
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, out[split + 4..].to_vec())
    }

    /// Page, list, download and upload over a real server on localhost.
    #[test]
    fn serves_shelf_over_http() {
        let root = std::env::temp_dir().join(format!("lumen-phone-http-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let shared = root.join("rapporto è.txt");
        std::fs::write(&shared, b"contenuto del file").unwrap();
        let id = crate::shelf::test_put(&shared);

        let received = Arc::new(Mutex::new(Vec::<(PathBuf, u64)>::new()));
        let rec2 = received.clone();
        let ctx = Arc::new(Ctx {
            token: "abc123".into(),
            dir: root.join("in"),
            on_received: Box::new(move |p: &Path, n: u64| rec2.lock().unwrap().push((p.to_path_buf(), n))),
        });
        let server = Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let t = std::thread::spawn(move || serve(server, ctx, stop2, Duration::from_secs(30)));

        // wrong or missing token: nothing
        assert_eq!(http(port, "GET", "/", b"").0, 404);
        assert_eq!(http(port, "GET", "/wrong/list.json", b"").0, 404);
        assert_eq!(http(port, "GET", &format!("/wrong/f/{id}"), b"").0, 404);
        // page and list
        let (s, page) = http(port, "GET", "/abc123", b"");
        assert_eq!(s, 200);
        assert!(String::from_utf8_lossy(&page).contains("Mensola di Lumen"));
        let (s, list) = http(port, "GET", "/abc123/list.json", b"");
        assert_eq!(s, 200);
        assert!(String::from_utf8_lossy(&list).contains("rapporto è.txt"));
        // download
        let (s, body) = http(port, "GET", &format!("/abc123/f/{id}"), b"");
        assert_eq!((s, body.as_slice()), (200, b"contenuto del file".as_slice()));
        assert_eq!(http(port, "GET", "/abc123/f/999999", b"").0, 404);
        // upload, also with a name that tries to leave the folder
        assert_eq!(http(port, "PUT", "/abc123/up?name=foto%20mare.jpg", b"JPEGDATA").0, 200);
        assert_eq!(http(port, "PUT", "/abc123/up?name=..%2F..%2Fevil.exe", b"X").0, 200);
        let got = received.lock().unwrap().clone();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, root.join("in").join("foto mare.jpg"));
        assert_eq!(std::fs::read(&got[0].0).unwrap(), b"JPEGDATA");
        assert_eq!(got[1].0, root.join("in").join("evil.exe"));
        assert!(!root.join("evil.exe").exists());

        stop.store(true, Ordering::SeqCst);
        t.join().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tokens_are_random() {
        assert_eq!(new_token().unwrap().len(), 32);
        assert_ne!(new_token().unwrap(), new_token().unwrap());
    }
}

#[cfg(test)]
mod lan_tests {
    /// Prints the address the QR code would use on this PC: cargo test lan -- --ignored --nocapture
    #[test]
    #[ignore]
    fn show_lan_address() {
        println!("LAN: {:?}", super::lan_address());
        for (n, ip) in local_ip_address::list_afinet_netifas().unwrap() {
            println!("  {n}: {ip}");
        }
    }
}

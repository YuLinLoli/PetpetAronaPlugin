//! 假 OneBot v11 实现，用于给 petpet 插件压端到端内存：
//! - 127.0.0.1:8080 按需提供 /img/<n>.png（每张不同的噪声色块，尺寸可配）
//! - 127.0.0.1:6700 正向 WS，框架连上来后分两批灌入群消息（默认各 100 条）
//! 每批结束写 marker 文件，外部脚本据此采样框架进程内存。
//!
//! 环境变量：ROUNDS1 / ROUNDS2 / IMG_SIZE / OUTDIR

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

const GROUP_ID: i64 = 123456789;
const USER_ID: i64 = 10001;
const SELF_ID: i64 = 99999;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 生成 n 号噪声图（size×size），PNG 编码；不同 n 图案不同
fn make_png(n: u64, size: u32) -> Vec<u8> {
    let mut img = image::RgbaImage::new(size, size);
    let base = (n as u32).wrapping_mul(2654435761);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let v = (x.wrapping_mul(7).wrapping_add(y.wrapping_mul(13)).wrapping_add(base)) as u8;
        let w = (x.wrapping_mul(31).wrapping_add(y.wrapping_mul(17)).wrapping_add(n as u32)) as u8;
        let z = (x ^ y ^ base) as u8;
        *px = image::Rgba([v, w, z, 255]);
    }
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("png 编码失败");
    out
}

/// 极简 HTTP 服务：GET /img/<n>.png
async fn http_server(port: u16, size: u32, hits: Arc<AtomicU64>) {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.expect("HTTP 端口占用");
    let cache: Arc<Mutex<HashMap<u64, Arc<Vec<u8>>>>> = Arc::new(Mutex::new(HashMap::new()));
    println!("[http] 127.0.0.1:{port} 就绪 (img {size}x{size})");
    loop {
        let Ok((mut socket, _)) = listener.accept().await else { continue };
        let cache = cache.clone();
        let hits = hits.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let Ok(nread) = socket.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..nread]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let n: u64 = path
                .trim_start_matches("/img/")
                .trim_end_matches(".png")
                .parse()
                .unwrap_or(0);
            let bytes = {
                let cached = cache.lock().unwrap().get(&n).cloned();
                match cached {
                    Some(bytes) => bytes,
                    None => {
                        let bytes = Arc::new(make_png(n, size));
                        cache.lock().unwrap().insert(n, bytes.clone());
                        bytes
                    }
                }
            };
            hits.fetch_add(1, Ordering::Relaxed);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = socket.write_all(header.as_bytes()).await;
            let _ = socket.write_all(&bytes).await;
            let _ = socket.flush().await;
        });
    }
}

fn image_event(n: u64, port: u16, template: &str, mode: &str, size: u32, text: &str) -> Value {
    use base64::Engine as _;
    let url = if mode == "base64" {
        let b64 = base64::engine::general_purpose::STANDARD.encode(make_png(n, size));
        format!("base64://{b64}")
    } else {
        format!("http://127.0.0.1:{port}/img/{n}.png")
    };
    json!({
        "post_type": "message",
        "message_type": "group",
        "sub_type": "normal",
        "message_id": 1000 + n as i64,
        "group_id": GROUP_ID,
        "user_id": USER_ID,
        "self_id": SELF_ID,
        "time": now_secs(),
        "font": 0,
        "raw_message": format!("[CQ:image,file={url}] {text}"),
        "sender": {
            "user_id": USER_ID,
            "nickname": "tester",
            "card": "",
            "role": "member",
            "sex": "unknown",
            "age": 0
        },
        "message": [
            { "type": "image", "data": { "file": url, "url": url } },
            { "type": "text", "data": { "text": text } }
        ]
    })
}

fn text_event(text: &str) -> Value {
    json!({
        "post_type": "message",
        "message_type": "group",
        "sub_type": "normal",
        "message_id": 9000,
        "group_id": GROUP_ID,
        "user_id": USER_ID,
        "self_id": SELF_ID,
        "time": now_secs(),
        "font": 0,
        "raw_message": text,
        "sender": {
            "user_id": USER_ID,
            "nickname": "tester",
            "card": "",
            "role": "admin",
            "sex": "unknown",
            "age": 0
        },
        "message": [ { "type": "text", "data": { "text": text } } ]
    })
}

fn poke_event() -> Value {
    json!({
        "post_type": "notice",
        "notice_type": "notify",
        "sub_type": "poke",
        "group_id": GROUP_ID,
        "user_id": USER_ID,
        "target_id": SELF_ID,
        "operator_id": USER_ID,
        "self_id": SELF_ID,
        "time": now_secs(),
        "sender": {
            "user_id": USER_ID,
            "nickname": "tester",
            "card": "",
            "role": "member",
            "sex": "unknown",
            "age": 0
        }
    })
}

fn action_reply(action: &str, echo: &Value) -> Value {
    let data = match action {
        "get_login_info" => json!({ "user_id": SELF_ID, "nickname": "Arona" }),
        "get_group_list" => json!([{ "group_id": GROUP_ID, "group_name": "test" }]),
        "get_group_info" | "get_group_detail_info" => {
            json!({ "group_id": GROUP_ID, "group_name": "test", "member_count": 3 })
        }
        "get_status" => json!({ "online": true, "good": true }),
        "send_group_msg" | "send_private_msg" | "send_msg" => json!({ "message_id": 5555 }),
        "send_forward_msg" => json!({ "message_id": 5556, "res_id": "res-1" }),
        _ => json!({}),
    };
    json!({ "status": "ok", "retcode": 0, "data": data, "echo": echo })
}

fn marker(outdir: &str, name: &str) {
    let _ = std::fs::write(format!("{outdir}/{name}"), b"1");
}

async fn wait_file(outdir: &str, name: &str) {
    let path = format!("{outdir}/{name}");
    while !std::path::Path::new(&path).exists() {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn send_round(
    tx: &mpsc::UnboundedSender<Message>,
    replies: &Arc<AtomicU64>,
    n: u64,
    port: u16,
    template: &str,
    mode: &str,
    size: u32,
    text: &str,
    wait: bool,
    poke: bool,
) {
    let target = n + 1;
    let event = if poke {
        poke_event()
    } else {
        image_event(n, port, template, mode, size, text)
    };
    if tx.send(Message::Text(event.to_string())).is_err() {
        panic!("WS 写通道已关闭");
    }
    if !wait {
        let ms: u64 = std::env::var("SLEEP_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        return;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    loop {
        if replies.load(Ordering::Relaxed) >= target {
            return;
        }
        if std::time::Instant::now() > deadline {
            println!("[warn] 第 {n} 轮 90s 内没等到回图，继续");
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::main]
async fn main() {
    let rounds1: u64 = std::env::var("ROUNDS1").ok().and_then(|v| v.parse().ok()).unwrap_or(100);
    let batches: u64 = std::env::var("BATCHES").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let size: u32 = std::env::var("IMG_SIZE").ok().and_then(|v| v.parse().ok()).unwrap_or(600);
    let outdir = std::env::var("OUTDIR").unwrap_or_else(|_| ".".to_string());
    let template = std::env::var("TEMPLATE").unwrap_or_else(|_| "memtest".to_string());
    let image_mode = std::env::var("IMAGE_MODE").unwrap_or_else(|_| "url".to_string());
    let text_override = std::env::var("TEXT").unwrap_or_default();
    let wait_reply = std::env::var("WAIT").ok().map(|v| v != "0").unwrap_or(true);
    let poke = std::env::var("POKE").ok().map(|v| v != "0").unwrap_or(false);
    let precmd = std::env::var("PRECMD").unwrap_or_default();
    let img_port = 8080u16;
    let ws_port = 6700u16;

    let hits = Arc::new(AtomicU64::new(0));
    tokio::spawn(http_server(img_port, size, hits.clone()));

    let listener = TcpListener::bind(("127.0.0.1", ws_port)).await.expect("WS 端口占用");
    println!("[ws] 127.0.0.1:{ws_port} 就绪，等待框架连接…");
    let (stream, _) = listener.accept().await.expect("accept 失败");
    let ws = tokio_tungstenite::accept_async(stream).await.expect("WS 握手失败");
    println!("[ws] 框架已连接");
    let (mut sink, mut stream) = ws.split();

    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    let replies = Arc::new(AtomicU64::new(0));
    let actions_seen = Arc::new(AtomicU64::new(0));
    let reader_replies = replies.clone();
    let reader_actions = actions_seen.clone();
    let reader_tx = tx.clone();
    let reader = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            let text = match msg {
                Message::Text(t) => t.to_string(),
                Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
                Message::Close(_) => break,
                _ => continue,
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
            let Some(action) = value.get("action").and_then(|v| v.as_str()) else { continue };
            let echo = value.get("echo").cloned().unwrap_or(Value::Null);
            reader_actions.fetch_add(1, Ordering::Relaxed);
            if matches!(action, "send_group_msg" | "send_forward_msg") {
                reader_replies.fetch_add(1, Ordering::Relaxed);
            }
            let _ = reader_tx.send(Message::Text(action_reply(action, &echo).to_string()));
        }
    });

    // 等外部先采一个干净基线（batch0.go），再开始灌
    wait_file(&outdir, "batch0.go").await;

    // 可选：先发一条普通群消息（例如 "pet nudge off"），再开始戳
    if !precmd.is_empty() {
        let _ = tx.send(Message::Text(text_event(&precmd).to_string()));
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    println!(
        "[base] replies={} actions={}",
        replies.load(Ordering::Relaxed),
        actions_seen.load(Ordering::Relaxed)
    );

    // 逐批灌入：每批 ROUNDS1 条，批间等外部采样完再继续（读 batchN.go）
    let mut sent: u64 = 0;
    for batch in 1..=batches {
        let t = std::time::Instant::now();
        for _ in 0..rounds1 {
            let text = if text_override.is_empty() { format!("pet {template}") } else { text_override.clone() };
            send_round(&tx, &replies, sent, img_port, &template, &image_mode, size, &text, wait_reply, poke).await;
            sent += 1;
            if sent % 20 == 0 {
                println!("[batch{batch}] 已发 {sent}");
            }
        }
        println!(
            "[batch{batch}] 完成，累计 {sent} 条，用时 {:?}，HTTP 命中 {}，动作 {}，回图 {}",
            t.elapsed(),
            hits.load(Ordering::Relaxed),
            actions_seen.load(Ordering::Relaxed),
            replies.load(Ordering::Relaxed)
        );
        marker(&outdir, &format!("batch{batch}.done"));
        if batch < batches {
            wait_file(&outdir, &format!("batch{batch}.go")).await;
        }
    }

    // 保持连接，等外部采样完再被杀
    println!(
        "[stat] replies={} actions={}",
        replies.load(Ordering::Relaxed),
        actions_seen.load(Ordering::Relaxed)
    );
    println!("[done] 进入待机");
    let _ = writer.await;
    let _ = reader.await;
}
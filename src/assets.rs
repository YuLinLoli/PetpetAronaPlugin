//! 图像资源加载：本地文件、帧序列目录、网络 URL，带一层进程内缓存。

use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, RgbaImage};

/// 一组帧 + 每帧延时（毫秒）
pub struct Frames {
    pub frames: Vec<RgbaImage>,
    pub delays: Vec<u32>,
}

impl Frames {
    pub fn single(image: RgbaImage) -> Frames {
        Frames {
            frames: vec![image],
            delays: vec![0],
        }
    }

    pub fn sample(&self, index: usize) -> &RgbaImage {
        if self.frames.is_empty() {
            // 不会发生：构造时至少一帧
            unreachable!("Frames 至少包含一帧")
        }
        &self.frames[index % self.frames.len()]
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }
}

/// LRU 顺序表：把 key 移到队尾（上限只有几百条，线性查找的开销可忽略）
fn touch(order: &mut VecDeque<String>, key: &str) {
    if let Some(pos) = order.iter().position(|item| item == key) {
        order.remove(pos);
    }
    order.push_back(key.to_string());
}

/// 一组帧占用的堆内存（像素 + 帧延时），用于按字节计量缓存
fn frames_bytes(frames: &Frames) -> usize {
    frames
        .frames
        .iter()
        .map(|frame| frame.as_raw().len())
        .sum::<usize>()
        + frames.delays.len() * std::mem::size_of::<u32>()
}
/// 进程内资源缓存（插件自己的内存，随插件停用一起释放）
pub struct AssetStore {
    cache: Mutex<Cache>,
}

struct Cache {
    map: HashMap<String, Entry>,
    order: VecDeque<String>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl Cache {
    /// 条数与总字节谁先超就淘汰最旧的；至少留一条，避免超大资源把自己也挤掉
    fn evict(&mut self) {
        while self.map.len() > 1
            && (self.map.len() > self.max_entries || self.bytes > self.max_bytes)
        {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(entry) = self.map.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
            }
        }
    }
}

struct Entry {
    frames: Arc<Frames>,
    bytes: usize,
}

impl AssetStore {
    /// 只限条数（测试与小对象场景够用）；`max_bytes` 用 [`AssetStore::with_limits`] 设
    pub fn new(cap: usize) -> AssetStore {
        AssetStore::with_limits(cap, usize::MAX)
    }

    /// `max_entries` 条 + `max_bytes` 字节双重上限，谁先到就按 LRU 淘汰最旧的
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> AssetStore {
        AssetStore {
            cache: Mutex::new(Cache {
                map: HashMap::new(),
                order: VecDeque::new(),
                bytes: 0,
                max_entries: max_entries.max(1),
                max_bytes,
            }),
        }
    }

    /// 运行时调整缓存上限并立即按新上限淘汰
    pub fn set_limits(&self, max_entries: usize, max_bytes: usize) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.max_entries = max_entries.max(1);
            cache.max_bytes = max_bytes;
            cache.evict();
        }
    }

    fn get(&self, key: &str) -> Option<Arc<Frames>> {
        let mut cache = self.cache.lock().ok()?;
        let frames = cache.map.get(key)?.frames.clone();
        touch(&mut cache.order, key);
        Some(frames)
    }

    fn put(&self, key: String, frames: Arc<Frames>) {
        let size = frames_bytes(&frames);
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        if let Some(previous) = cache.map.insert(
            key.clone(),
            Entry {
                frames,
                bytes: size,
            },
        ) {
            cache.bytes = cache.bytes.saturating_sub(previous.bytes);
        }
        cache.bytes += size;
        touch(&mut cache.order, &key);
        cache.evict();
    }

    /// 加载一个资源地址：网络 URL / 本地文件 / 帧序列目录 / 空串（返回 None）
    pub async fn load(&self, base: &Path, src: &str) -> Result<Option<Arc<Frames>>, String> {
        let trimmed = src.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        // 内联图（`data:...;base64,...`）不落盘也不进缓存：解出来直接用，随请求一起释放
        if let Some(payload) = trimmed.strip_prefix("data:") {
            return Ok(Some(Arc::new(decode_data_uri(payload)?)));
        }
        // 缓存键用解析后的绝对路径，避免不同模板目录下的同名资源互相串缓存
        let is_url = trimmed.starts_with("http://") || trimmed.starts_with("https://");
        let cache_key = if is_url {
            trimmed.to_string()
        } else {
            resolve_local(base, trimmed).to_string_lossy().to_string()
        };
        if let Some(frames) = self.get(&cache_key) {
            return Ok(Some(frames));
        }
        let frames = if is_url {
            let bytes = arona::runtime::http::send(arona::runtime::http::Request::get(trimmed))
                .await
                .map_err(|err| format!("下载 {trimmed} 失败: {err}"))?;
            decode_bytes(trimmed, &bytes)?
        } else {
            load_local(Path::new(&cache_key))?
        };
        let frames = Arc::new(frames);
        self.put(cache_key, frames.clone());
        Ok(Some(frames))
    }
}

/// 把内存里的图像字节编码成 `data:` URI：给内联输入图用，不落盘、不进缓存
pub fn data_uri(mime: &str, bytes: &[u8]) -> String {
    use base64::Engine as _;
    let mime = if mime.is_empty() { "application/octet-stream" } else { mime };
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// 解析 `data:[<mime>][;base64],<payload>`（`data:` 之后的部分）
fn decode_data_uri(payload: &str) -> Result<Frames, String> {
    use base64::Engine as _;
    let (meta, data) = payload
        .split_once(',')
        .ok_or_else(|| "内联图像 data URI 缺少逗号".to_string())?;
    let bytes = if meta.trim_end().ends_with(";base64") {
        base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .map_err(|err| format!("解析内联图像 base64 失败: {err}"))?
    } else {
        data.as_bytes().to_vec()
    };
    decode_bytes("内联图像", &bytes)
}

/// `file://` 前缀与相对路径解析
pub fn resolve_local(base: &Path, src: &str) -> PathBuf {
    let cleaned = src.trim().trim_start_matches("file://");
    let cleaned = cleaned.strip_prefix("./").unwrap_or(cleaned);
    let path = Path::new(cleaned);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn load_local(path: &Path) -> Result<Frames, String> {
    if path.is_dir() {
        load_frame_dir(path)
    } else if path.is_file() {
        let bytes =
            std::fs::read(path).map_err(|err| format!("读取 {} 失败: {err}", path.display()))?;
        decode_bytes(&path.to_string_lossy(), &bytes)
    } else {
        Err(format!("资源不存在: {}", path.display()))
    }
}

/// 帧序列目录：按文件名里的数字排序（0.png、1.png ...）
fn load_frame_dir(dir: &Path) -> Result<Frames, String> {
    let mut entries: Vec<(i64, PathBuf)> = Vec::new();
    let reader = std::fs::read_dir(dir).map_err(|err| format!("读取目录 {} 失败: {err}", dir.display()))?;
    for entry in reader.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_image = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif"
                )
            })
            .unwrap_or(false);
        if !is_image {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let key = stem.parse::<i64>().unwrap_or(i64::MAX);
        entries.push((key, path));
    }
    if entries.is_empty() {
        return Err(format!("目录 {} 里没有图像帧", dir.display()));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut frames = Vec::new();
    for (_, path) in entries {
        let bytes = std::fs::read(&path).map_err(|err| format!("读取 {} 失败: {err}", path.display()))?;
        let single = decode_bytes(&path.to_string_lossy(), &bytes)?;
        frames.push(single.frames.into_iter().next().unwrap());
    }
    let count = frames.len();
    Ok(Frames {
        frames,
        delays: vec![0; count],
    })
}

/// 按内容/扩展名解码：GIF 取全部帧，其它取单帧
pub fn decode_bytes(name: &str, bytes: &[u8]) -> Result<Frames, String> {
    let is_gif = bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || name.to_ascii_lowercase().ends_with(".gif");
    if is_gif {
        let decoder = GifDecoder::new(Cursor::new(bytes))
            .map_err(|err| format!("解析 GIF {name} 失败: {err}"))?;
        let frames = decoder
            .into_frames()
            .collect_frames()
            .map_err(|err| format!("解码 GIF 帧 {name} 失败: {err}"))?;
        if frames.is_empty() {
            return Err(format!("GIF {name} 没有帧"));
        }
        let mut images = Vec::with_capacity(frames.len());
        let mut delays = Vec::with_capacity(frames.len());
        for frame in frames {
            let (numer, denom) = frame.delay().numer_denom_ms();
            delays.push(numer.checked_div(denom).unwrap_or(0));
            images.push(frame.into_buffer());
        }
        return Ok(Frames {
            frames: images,
            delays,
        });
    }
    let image = image::load_from_memory(bytes)
        .map_err(|err| format!("解码图像 {name} 失败: {err}"))?
        .to_rgba8();
    Ok(Frames::single(image))
}

/// QQ 头像地址（与 petpet 一致的头像接口）
pub fn avatar_url(qq: i64, max_size: u32) -> String {
    const LEVELS: [u32; 4] = [40, 100, 140, 640];
    let spec = if max_size == 0 {
        640
    } else {
        let mut chosen = 640;
        for level in LEVELS {
            if level >= max_size {
                chosen = level;
                break;
            }
        }
        chosen
    };
    format!("https://q.qlogo.cn/headimg_dl?dst_uin={qq}&spec={spec}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    fn solid(width: u32, height: u32) -> Arc<Frames> {
        Arc::new(Frames::single(RgbaImage::new(width, height)))
    }

    /// 每张 10x10 RGBA 约 400 字节；字节上限到了就淘汰最旧的
    #[test]
    fn evicts_oldest_when_bytes_exceeded() {
        let store = AssetStore::with_limits(64, 1000);
        store.put("a".to_string(), solid(10, 10));
        store.put("b".to_string(), solid(10, 10));
        store.put("c".to_string(), solid(10, 10));
        assert!(store.get("a").is_none());
        assert!(store.get("b").is_some());
        assert!(store.get("c").is_some());
    }

    #[test]
    fn evicts_oldest_when_entries_exceeded() {
        let store = AssetStore::with_limits(2, usize::MAX);
        store.put("a".to_string(), solid(1, 1));
        store.put("b".to_string(), solid(1, 1));
        store.put("c".to_string(), solid(1, 1));
        assert!(store.get("a").is_none());
        assert!(store.get("b").is_some());
        assert!(store.get("c").is_some());
    }

    /// 热重载收紧上限后立即裁剪，至少保留最新一条
    #[test]
    fn set_limits_trims_existing_cache() {
        let store = AssetStore::with_limits(64, usize::MAX);
        store.put("a".to_string(), solid(10, 10));
        store.put("b".to_string(), solid(10, 10));
        store.put("c".to_string(), solid(10, 10));
        store.set_limits(64, 1);
        assert!(store.get("a").is_none());
        assert!(store.get("b").is_none());
        assert!(store.get("c").is_some());
    }

    /// 内联输入图：编码成 data URI 后能原地解回帧
    #[test]
    fn data_uri_round_trips_image() {
        use image::ImageEncoder as _;
        use image::codecs::png::PngEncoder;
        let img = RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]));
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(img.as_raw(), 4, 4, image::ExtendedColorType::Rgba8)
            .unwrap();
        let uri = data_uri("image/png", &png);
        assert!(uri.starts_with("data:image/png;base64,"));
        let frames = decode_data_uri(uri.strip_prefix("data:").unwrap()).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames.sample(0).dimensions(), (4, 4));
    }

    #[test]
    fn data_uri_rejects_malformed() {
        assert!(decode_data_uri("image/png;base64").is_err());
    }
}
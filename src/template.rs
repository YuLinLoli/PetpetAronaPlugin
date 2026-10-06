//! 模板模型与解析：兼容新版 `template.json` 与旧版 `data.json`（移植 petpet-core 的模板子集）。
//!
//! 只解析、不渲染；渲染在 [`crate::render`]。这里保持纯数据，便于单测。

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::length::{self, Node, Offset};

/// 模板类型
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Gif,
}

/// 画布配置
#[derive(Clone, Debug)]
pub struct Canvas {
    pub width: Option<Node>,
    pub height: Option<Node>,
    pub length: Option<Node>,
    pub color: [u8; 4],
    pub reverse: bool,
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas {
            width: None,
            height: None,
            length: None,
            color: [255, 255, 255, 0],
            reverse: false,
        }
    }
}

/// 图像适应方式
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    Fill,
    Cover,
    Contain,
    None,
}

/// 圆角
#[derive(Clone, Debug)]
pub enum RadiusSpec {
    None,
    Uniform(Node),
    Corners([Node; 4]),
}

/// 支持的滤镜（对齐 petpet 的 `ImageFilterTemplate` 全集）
#[derive(Clone, Debug)]
pub enum Filter {
    Gray,
    Binarize,
    Mirror,
    Flip,
    Blur(f32),
    Contrast { brightness: f32, contrast: f32 },
    Hsb { hue: f32, saturation: f32, brightness: f32 },
    Noise(f32),
    Swirl { radius: f32, angle: f32, x: f32, y: f32 },
    Bulge { radius: f32, strength: f32, x: f32, y: f32 },
    Swim { scale: f32, stretch: f32, angle: f32, amount: f32, turbulence: f32, time: f32 },
    Halftone { radius: f32, angle: f32 },
    DotScreen { radius: f32, angle: f32 },
    Denoise,
    Oil { skip: i32, range: i32, levels: i32 },
    SwingTwist { max_angle: f32, amplitude: f32, omega: f32, decay: f32, index_offset: i32 },
    Mirage(MirageSpec),
}

/// `mirage` 滤镜参数：需要再取一张图作为「内层」底图，再按亮度做融合
#[derive(Clone, Debug)]
pub struct MirageSpec {
    /// 请求键（取图用）
    pub keys: Vec<String>,
    /// 模板内默认图
    pub srcs: Vec<String>,
    pub inner_scale: f32,
    pub cover_scale: f32,
    pub inner_desat: f32,
    pub cover_desat: f32,
    pub weight: f32,
    pub max_size: i32,
    pub colored: bool,
}

/// 图像元素（对应 AvatarTemplate / static image）
#[derive(Clone, Debug)]
pub struct ImageElement {
    pub id: Option<String>,
    /// 与请求数据匹配的键（`to`/`from`/`0` ...）
    pub keys: Vec<String>,
    /// 默认图像地址（`default`/`src`），可为一组动画帧
    pub srcs: Vec<String>,
    /// 每帧坐标（单帧时长度为 1）
    pub coords: Vec<Coords>,
    pub crop: Option<Crop>,
    pub fit: Fit,
    pub position: Offset,
    pub angle: Vec<f32>,
    pub opacity: Vec<f32>,
    pub radius: Vec<RadiusSpec>,
    pub filters: Vec<Filter>,
    /// 旋转中心（`DEFAULT` -> 左上，`CENTER` -> 中心）
    pub origin: Offset,
    /// 逐帧旋转动画（旧模板 `rotate: true` -> 0..360 线性）
    pub rotate: Option<RotateTransition>,
    pub start: i64,
    pub end: i64,
    pub reverse: bool,
}

/// 文本元素
#[derive(Clone, Debug)]
pub struct TextElement {
    pub id: Option<String>,
    pub text: Vec<String>,
    pub coords: Vec<TextCoords>,
    pub color: Vec<[u8; 4]>,
    pub font: Vec<String>,
    pub size: Vec<f32>,
    pub max_size: Vec<f32>,
    pub align: Align,
    pub baseline: Baseline,
    pub wrap: Wrap,
    pub angle: Vec<f32>,
    pub opacity: Vec<f32>,
    pub stroke_color: Option<[u8; 4]>,
    pub stroke_size: f32,
    pub style: TextStyleKind,
    pub origin: Offset,
    /// 只在这些帧上绘制（对齐 petpet 的 `start`/`end`，`end < 0` 表示到末尾）
    pub start: i64,
    pub end: i64,
}

/// 背景元素（覆盖画布配置）
#[derive(Clone, Debug)]
pub struct BackgroundElement {
    pub srcs: Vec<String>,
    pub reverse: bool,
}

#[derive(Clone, Debug)]
pub enum Element {
    Image(ImageElement),
    Text(TextElement),
    Background(BackgroundElement),
}

/// 图像坐标：XYWH 矩形，或 P4A（4 个角点 + 1 个绘制锚点）透视变形
#[derive(Clone, Debug)]
pub enum Coords {
    /// 该帧不绘制（JSON 里的 `null`）
    Empty,
    Xywh { x: Node, y: Node, w: Node, h: Node },
    P4a { points: [[Node; 2]; 4], anchor: [Node; 2] },
}

/// 文本坐标 `[x, y, max_width?]`
#[derive(Clone, Debug)]
pub struct TextCoords {
    pub x: Node,
    pub y: Node,
    pub max_w: Option<Node>,
}

/// 裁切区域：(x1,y1)-(x2,y2) 矩形，或多边形（相对元素尺寸的百分比）
#[derive(Clone, Debug)]
pub enum Crop {
    Xywh { x1: Node, y1: Node, x2: Node, y2: Node },
    Polygon(Vec<[Node; 2]>),
}

/// 文字样式：粗体/斜体由渲染层合成
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextStyleKind {
    Plain,
    Bold,
    Italic,
    BoldItalic,
}

/// 缓动函数（对齐 petpet 的 `EasingFunction`）
#[derive(Clone, Copy, Debug)]
pub enum Easing {
    Linear,
    Cubic([f32; 4]),
}

impl Easing {
    fn easing(&self, t: f32) -> f32 {
        match self {
            Easing::Linear => t,
            Easing::Cubic(params) => cubic_bezier(*params, t),
        }
    }
}

/// 逐帧旋转动画（旧模板 `rotate: true` => start 0 / end 360 / 线性）
#[derive(Clone, Debug)]
pub struct RotateTransition {
    pub start: f32,
    pub end: f32,
    pub count: i32,
    pub easing: Easing,
}

impl RotateTransition {
    /// 对齐 petpet `RotateTransition::getAngle(length, index)`：长度按帧数 + 1 传入
    pub fn angle(&self, frames: usize, index: usize) -> f32 {
        let total = (frames + 1) as f32;
        let end = self.end + 360.0 * self.count as f32;
        match self.easing {
            Easing::Linear => {
                if frames == 0 {
                    self.start
                } else {
                    self.start + (end - self.start) * index as f32 / frames as f32
                }
            }
            Easing::Cubic(_) => {
                if index == 0 {
                    self.start
                } else if index as f32 >= total {
                    end
                } else {
                    self.start
                        + (end - self.start) * self.easing.easing(index as f32 / total)
                }
            }
        }
    }
}

/// 标准 cubic-bezier(x1,y1,x2,y2) 求解（WebKit UnitBezier 的牛顿迭代）
fn cubic_bezier(p: [f32; 4], x: f32) -> f32 {
    let [x1, y1, x2, y2] = p;
    let cx = 3.0 * x1;
    let bx = 3.0 * (x2 - x1) - cx;
    let ax = 1.0 - cx - bx;
    let cy = 3.0 * y1;
    let by = 3.0 * (y2 - y1) - cy;
    let ay = 1.0 - cy - by;
    let sample_x = |t: f32| ((ax * t + bx) * t + cx) * t;
    let sample_y = |t: f32| ((ay * t + by) * t + cy) * t;
    let sample_dx = |t: f32| (3.0 * ax * t + 2.0 * bx) * t + cx;
    let mut t = x;
    for _ in 0..8 {
        let error = sample_x(t) - x;
        if error.abs() < 1e-6 {
            return sample_y(t);
        }
        let derivative = sample_dx(t);
        if derivative.abs() < 1e-6 {
            break;
        }
        t -= error / derivative;
    }
    let (mut low, mut high) = (0.0f32, 1.0f32);
    t = x;
    for _ in 0..24 {
        let value = sample_x(t);
        if (value - x).abs() < 1e-6 {
            break;
        }
        if value < x {
            low = t;
        } else {
            high = t;
        }
        t = (low + high) / 2.0;
    }
    sample_y(t)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Baseline {
    Top,
    /// y 为文字基线（petpet 旧模板默认 ALPHABETIC）
    Alphabetic,
    Middle,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrap {
    None,
    Break,
    Zoom,
}

/// 完整模板
#[derive(Clone, Debug)]
pub struct Template {
    pub id: String,
    pub dir: PathBuf,
    pub kind: Kind,
    pub canvas: Canvas,
    pub elements: Vec<Element>,
    /// 帧间延时（毫秒）
    pub delays: Vec<u32>,
    pub aliases: Vec<String>,
    pub description: Option<String>,
    pub in_random_list: bool,
    pub default_weight: i64,
    pub hidden: bool,
    pub preview: Option<String>,
    pub image_keys: BTreeSet<String>,
    pub text_keys: BTreeSet<String>,
    pub avatar_sizes: HashMap<String, u32>,
}

impl Template {
    pub fn base_delay(&self) -> u32 {
        self.delays.first().copied().unwrap_or(65)
    }

    pub fn is_gif(&self) -> bool {
        self.kind == Kind::Gif
    }
}

/// 从模板目录加载（优先 template.json，其次 data.json）
pub fn load_dir(id: &str, dir: &Path) -> Result<Template, String> {
    let new_file = dir.join("template.json");
    let old_file = dir.join("data.json");
    if new_file.is_file() {
        let text = std::fs::read_to_string(&new_file)
            .map_err(|err| format!("读取 {} 失败: {err}", new_file.display()))?;
        // 有些模板带 UTF-8 BOM，serde_json 不认，先剥掉
        let text = text.trim_start_matches('\u{feff}');
        let json: Value = serde_json::from_str(text)
            .map_err(|err| format!("解析 {} 失败: {err}", new_file.display()))?;
        parse_new(id, dir, &json).map_err(|err| format!("模板 {id}: {err}"))
    } else if old_file.is_file() {
        let text = std::fs::read_to_string(&old_file)
            .map_err(|err| format!("读取 {} 失败: {err}", old_file.display()))?;
        let text = text.trim_start_matches('\u{feff}');
        let json: Value = serde_json::from_str(text)
            .map_err(|err| format!("解析 {} 失败: {err}", old_file.display()))?;
        parse_old(id, dir, &json).map_err(|err| format!("模板 {id}: {err}"))
    } else {
        Err(format!("{} 下没有 template.json / data.json", dir.display()))
    }
}
// ==================== 新版 template.json ====================

fn parse_new(id: &str, dir: &Path, json: &Value) -> Result<Template, String> {
    let kind = match json.get("type").and_then(Value::as_str) {
        Some("gif") => Kind::Gif,
        _ => Kind::Image,
    };
    let metadata = json.get("metadata");
    let aliases = string_list(metadata.and_then(|m| m.get("alias")));
    let description = metadata
        .and_then(|m| m.get("description").or_else(|| m.get("desc")))
        .and_then(Value::as_str)
        .map(str::to_string);
    let in_random_list = metadata
        .and_then(|m| m.get("in_random_list"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let default_weight = metadata
        .and_then(|m| m.get("default_template_weight"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let hidden = metadata
        .and_then(|m| m.get("hidden"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let preview = metadata
        .and_then(|m| m.get("preview"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let canvas = parse_canvas(json.get("canvas"))?;

    let mut delays = Vec::new();
    if let Some(delay) = json.get("delay") {
        for item in items(delay) {
            if let Some(value) = item.as_i64() {
                delays.push(value.clamp(1, 65535) as u32);
            } else if let Some(value) = item.as_f64() {
                delays.push((value as i64).clamp(1, 65535) as u32);
            }
        }
    }
    if delays.is_empty()
        && let Some(fps) = json.get("fps").and_then(Value::as_f64)
            && fps > 0.0 {
                delays.push((1000.0 / fps).round().clamp(1.0, 65535.0) as u32);
            }
    if delays.is_empty() {
        delays.push(65);
    }

    let mut elements = Vec::new();
    if let Some(list) = json.get("elements").and_then(Value::as_array) {
        for element in list {
            match element.get("type").and_then(Value::as_str) {
                Some("image") | Some("avatar") => {
                    elements.push(Element::Image(parse_image(element)?));
                }
                Some("text") => {
                    let mut text = parse_text(element)?;
                    // 兼容旧式 `$txt1[默认]`：新版规范是 `${0:默认}`，但真实模板里两种写法都常见
                    for line in &mut text.text {
                        *line = convert_old_text_tokens(line, false);
                    }
                    elements.push(Element::Text(text));
                }
                Some("background") => {
                    elements.push(Element::Background(parse_background(element)?))
                }
                other => {
                    eprintln!("petpet: 跳过不支持的元素类型 {other:?}");
                }
            }
        }
    }

    let mut template = Template {
        id: id.to_string(),
        dir: dir.to_path_buf(),
        kind,
        canvas,
        elements,
        delays,
        aliases,
        description,
        in_random_list,
        default_weight,
        hidden,
        preview,
        image_keys: BTreeSet::new(),
        text_keys: BTreeSet::new(),
        avatar_sizes: HashMap::new(),
    };
    template.collect_request_keys();
    Ok(template)
}

fn parse_canvas(value: Option<&Value>) -> Result<Canvas, String> {
    let mut canvas = Canvas::default();
    let Some(value) = value else {
        return Ok(canvas);
    };
    canvas.width = value.get("width").map(length::from_json).transpose()?;
    canvas.height = value.get("height").map(length::from_json).transpose()?;
    canvas.length = value.get("length").map(length::from_json).transpose()?;
    if let Some(color) = value.get("color").and_then(Value::as_str)
        && let Some(rgba) = parse_color(color) {
            canvas.color = rgba;
        }
    canvas.reverse = value
        .get("reverse")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(canvas)
}

fn parse_image(value: &Value) -> Result<ImageElement, String> {
    let id = value.get("id").and_then(Value::as_str).map(str::to_string);
    let mut keys = string_list(value.get("key"));
    keys.retain(|key| !key.is_empty());
    if keys.is_empty() {
        keys.push("unknown".to_string());
    }
    let mut srcs = string_list(value.get("default"));
    srcs.extend(string_list(value.get("src")));
    srcs.retain(|s| !s.is_empty());

    let coords = parse_image_coords(value.get("coords"))?;
    let crop = parse_crop(value.get("crop"))?;
    let fit = match value.get("fit").and_then(Value::as_str) {
        Some("cover") => Fit::Cover,
        Some("contain") => Fit::Contain,
        Some("none") => Fit::None,
        _ => Fit::Fill,
    };
    let position = value
        .get("position")
        .and_then(Value::as_str)
        .map(length::parse_offset)
        .unwrap_or_default();

    Ok(ImageElement {
        id,
        keys,
        srcs,
        coords,
        crop,
        fit,
        position,
        angle: float_list(value.get("angle"), 0.0),
        opacity: float_list(value.get("opacity"), 1.0),
        radius: parse_radius_list(value.get("border_radius")),
        filters: parse_filters(value.get("filter"))?,
        origin: value
            .get("origin")
            .and_then(Value::as_str)
            .map(length::parse_offset)
            .unwrap_or_else(Offset::center),
        rotate: parse_rotate(value.get("rotate"))?,
        start: value.get("start").and_then(Value::as_i64).unwrap_or(0),
        end: value.get("end").and_then(Value::as_i64).unwrap_or(-1),
        reverse: value
            .get("reverse")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// 逐帧旋转：`true` / 缓动名 / `cubic-bezier(...)` / `{start,end,rotate_count,easing}`
fn parse_rotate(value: Option<&Value>) -> Result<Option<RotateTransition>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let default = RotateTransition {
        start: 0.0,
        end: 360.0,
        count: 0,
        easing: Easing::Linear,
    };
    match value {
        Value::Bool(false) | Value::Null => Ok(None),
        Value::Bool(true) => Ok(Some(default)),
        Value::String(name) => Ok(Some(RotateTransition {
            easing: parse_easing(name),
            ..default
        })),
        Value::Object(map) => {
            let number = |key: &str, fallback: f32| map.get(key).and_then(as_f32).unwrap_or(fallback);
            let easing = map
                .get("easing")
                .and_then(Value::as_str)
                .map(parse_easing)
                .unwrap_or(Easing::Linear);
            Ok(Some(RotateTransition {
                start: number("start", 0.0),
                end: number("end", 360.0),
                count: map
                    .get("rotate_count")
                    .and_then(Value::as_i64)
                    .unwrap_or(1) as i32,
                easing,
            }))
        }
        _ => Ok(None),
    }
}

fn parse_easing(name: &str) -> Easing {
    let lower = name.trim().to_ascii_lowercase();
    match lower.as_str() {
        "ease" => Easing::Cubic([0.25, 0.1, 0.25, 1.0]),
        "ease-in" => Easing::Cubic([0.42, 0.0, 1.0, 1.0]),
        "ease-out" => Easing::Cubic([0.0, 0.0, 0.58, 1.0]),
        "ease-in-out" => Easing::Cubic([0.42, 0.0, 0.58, 1.0]),
        other if other.starts_with("cubic-bezier") => {
            let inner = other
                .trim_start_matches("cubic-bezier")
                .trim()
                .trim_start_matches('(')
                .trim_end_matches(')');
            let values: Vec<f32> = inner
                .split(',')
                .filter_map(|item| item.trim().parse::<f32>().ok())
                .collect();
            match values.as_slice() {
                [a, b, c, d] => Easing::Cubic([*a, *b, *c, *d]),
                _ => Easing::Linear,
            }
        }
        _ => Easing::Linear,
    }
}

fn parse_text(value: &Value) -> Result<TextElement, String> {
    let id = value.get("id").and_then(Value::as_str).map(str::to_string);
    let mut text: Vec<String> = string_list(value.get("text"));
    if text.is_empty() {
        text.push(String::new());
    }
    let coords = parse_text_coords(value.get("coords"))?;
    let mut color: Vec<[u8; 4]> = items_of(value.get("color"))
        .into_iter()
        .filter_map(Value::as_str)
        .filter_map(parse_color)
        .collect();
    if color.is_empty() {
        color.push([255, 255, 255, 255]);
    }
    let mut font: Vec<String> = string_list(value.get("font"));
    font.retain(|f| !f.is_empty());
    let align = match value.get("align").and_then(Value::as_str) {
        Some("center") => Align::Center,
        Some("right") => Align::Right,
        _ => Align::Left,
    };
    let baseline = match value.get("baseline").and_then(Value::as_str) {
        Some("middle") => Baseline::Middle,
        Some("bottom") => Baseline::Bottom,
        Some("alphabetic") => Baseline::Alphabetic,
        _ => Baseline::Top,
    };
    let wrap = match value.get("wrap").and_then(Value::as_str) {
        Some("break") => Wrap::Break,
        Some("zoom") => Wrap::Zoom,
        _ => Wrap::None,
    };
    let stroke_color = value
        .get("stroke_color")
        .and_then(Value::as_str)
        .and_then(parse_color)
        .filter(|rgba| rgba[3] != 0);

    Ok(TextElement {
        id,
        text,
        coords,
        color,
        font,
        size: float_list(value.get("size").or_else(|| value.get("min_size")), 16.0),
        max_size: float_list(value.get("max_size"), 96.0),
        align,
        baseline,
        wrap,
        angle: float_list(value.get("angle"), 0.0),
        opacity: float_list(value.get("opacity"), 1.0),
        stroke_color,
        stroke_size: value.get("stroke_size").and_then(as_f32).unwrap_or(0.0),
        style: parse_text_style(value.get("style")),
        origin: value
            .get("origin")
            .and_then(Value::as_str)
            .map(length::parse_offset)
            .unwrap_or_else(Offset::center),
        start: value.get("start").and_then(Value::as_i64).unwrap_or(0),
        end: value.get("end").and_then(Value::as_i64).unwrap_or(-1),
    })
}

fn parse_text_style(value: Option<&Value>) -> TextStyleKind {
    match value.and_then(Value::as_str).map(|s| s.to_ascii_lowercase()) {
        Some(ref name) if name == "bold" => TextStyleKind::Bold,
        Some(ref name) if name == "italic" => TextStyleKind::Italic,
        Some(ref name) if name == "bold_italic" => TextStyleKind::BoldItalic,
        _ => TextStyleKind::Plain,
    }
}

fn parse_background(value: &Value) -> Result<BackgroundElement, String> {
    let mut srcs = string_list(value.get("src"));
    srcs.retain(|s| !s.is_empty());
    Ok(BackgroundElement {
        srcs,
        reverse: value
            .get("reverse")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}
fn parse_image_coords(value: Option<&Value>) -> Result<Vec<Coords>, String> {
    let Some(value) = value else {
        return Ok(vec![default_coords()]);
    };
    if value.is_null() {
        return Ok(vec![Coords::Empty]);
    }
    if is_coords_entry(value) {
        return Ok(vec![coords_entry(value)?]);
    }
    let list = value
        .as_array()
        .ok_or_else(|| "coords 既不是坐标也不是坐标数组".to_string())?;
    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        out.push(coords_entry(entry)?);
    }
    if out.is_empty() {
        out.push(default_coords());
    }
    Ok(out)
}

/// 一段值是不是"单个坐标"（XYWH 或 deform）
fn is_coords_entry(value: &Value) -> bool {
    let Some(list) = value.as_array() else {
        return false;
    };
    let Some(first) = list.iter().find(|item| !item.is_null()) else {
        return false;
    };
    match first {
        Value::Array(pair) => pair.len() == 2,
        Value::Object(_) => false,
        _ => true,
    }
}

pub fn default_coords() -> Coords {
    Coords::Xywh {
        x: Node::px(0.0),
        y: Node::px(0.0),
        w: Node::px(100.0),
        h: Node::px(100.0),
    }
}

fn coords_entry(value: &Value) -> Result<Coords, String> {
    if value.is_null() {
        return Ok(Coords::Empty);
    }
    let list = value
        .as_array()
        .ok_or_else(|| "坐标必须是数组".to_string())?;
    let is_point = |item: &Value| item.as_array().is_some_and(|pair| pair.len() == 2);
    if (4..=5).contains(&list.len()) && list[0].is_array() && list.iter().all(is_point) {
        // P4A：4 个角点 + 1 个绘制锚点（缺省用第 4 点）
        let mut points: Vec<[Node; 2]> = Vec::with_capacity(list.len());
        for point in list {
            let pair = point.as_array().expect("上面已判过是二元点");
            points.push([length::from_json(&pair[0])?, length::from_json(&pair[1])?]);
        }
        let anchor = points.get(4).cloned().unwrap_or_else(|| points[3].clone());
        return Ok(Coords::P4a {
            points: [
                points[0].clone(),
                points[1].clone(),
                points[2].clone(),
                points[3].clone(),
            ],
            anchor,
        });
    }
    let node = |index: usize| -> Result<Node, String> {
        Ok(list
            .get(index)
            .filter(|item| !item.is_null())
            .map(length::from_json)
            .transpose()?
            .unwrap_or_else(|| Node::px(0.0)))
    };
    Ok(Coords::Xywh {
        x: node(0)?,
        y: node(1)?,
        w: node(2)?,
        h: node(3)?,
    })
}

fn parse_text_coords(value: Option<&Value>) -> Result<Vec<TextCoords>, String> {
    let Some(value) = value else {
        return Ok(vec![default_text_coords()]);
    };
    if value.is_null() {
        return Ok(vec![default_text_coords()]);
    }
    if is_text_coords_entry(value) {
        return Ok(vec![text_coords_entry(value)?]);
    }
    let list = value
        .as_array()
        .ok_or_else(|| "text coords 必须是数组".to_string())?;
    let mut out = Vec::new();
    for entry in list {
        out.push(text_coords_entry(entry)?);
    }
    if out.is_empty() {
        out.push(default_text_coords());
    }
    Ok(out)
}

/// 文本坐标单帧的判定（petpet 的 `TextXYWCoordsListSerializer`）：首元素是数字/字符串才算单帧，
/// 与图像坐标"首元素是二元点也算单帧"的规则不同。
fn is_text_coords_entry(value: &Value) -> bool {
    let Some(list) = value.as_array() else {
        return false;
    };
    list.iter()
        .find(|item| !item.is_null())
        .is_some_and(|item| !item.is_array())
}

/// petpet 的 `TextXYWCoords.DEFAULT`：`[0, 0, 200]`
fn default_text_coords() -> TextCoords {
    TextCoords {
        x: Node::px(0.0),
        y: Node::px(0.0),
        max_w: Some(Node::px(200.0)),
    }
}

fn text_coords_entry(value: &Value) -> Result<TextCoords, String> {
    let list = value
        .as_array()
        .ok_or_else(|| "text 坐标必须是数组".to_string())?;
    if list.len() < 2 {
        return Err("text 坐标至少要有 x、y".to_string());
    }
    Ok(TextCoords {
        x: length::from_json(&list[0])?,
        y: length::from_json(&list[1])?,
        max_w: Some(match list.get(2) {
            Some(node) if !node.is_null() => length::from_json(node)?,
            _ => Node::px(200.0),
        }),
    })
}

/// 坐标的"名义尺寸"（只算绝对值）：XYWH 取 w/h，P4A 取四个角点的包围盒
pub fn coords_size(coords: &Coords) -> (f32, f32) {
    match coords {
        Coords::Empty => (0.0, 0.0),
        Coords::Xywh { w, h, .. } => (
            w.fixed_px().unwrap_or(0.0).abs(),
            h.fixed_px().unwrap_or(0.0).abs(),
        ),
        Coords::P4a { points, .. } => {
            let mut min_x = f32::INFINITY;
            let mut min_y = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;
            let mut max_y = f32::NEG_INFINITY;
            for point in points {
                let x = point[0].fixed_px().unwrap_or(0.0);
                let y = point[1].fixed_px().unwrap_or(0.0);
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
            if min_x.is_finite() && max_x.is_finite() {
                ((max_x - min_x).abs(), (max_y - min_y).abs())
            } else {
                (0.0, 0.0)
            }
        }
    }
}

fn parse_crop(value: Option<&Value>) -> Result<Option<Crop>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(list) = value.as_array().filter(|list| !list.is_empty()) else {
        return Ok(None);
    };
    if list[0].is_array() {
        let inner = list[0].as_array().expect("上面已判过是数组");
        if inner.first().is_some_and(Value::is_array) {
            // 多边形裁切（相对元素尺寸的百分比 / 像素）
            let mut points = Vec::with_capacity(inner.len());
            for point in inner {
                let pair = point
                    .as_array()
                    .ok_or_else(|| "裁切点必须是数组".to_string())?;
                if pair.len() < 2 {
                    return Err("裁切点缺少分量".to_string());
                }
                points.push([length::from_json(&pair[0])?, length::from_json(&pair[1])?]);
            }
            if points.len() < 3 {
                return Ok(None);
            }
            return Ok(Some(Crop::Polygon(points)));
        }
        return crop_from_values(inner);
    }
    crop_from_values(list)
}

/// `[w]` / `[w,h]` / `[x1,y1,x2,y2]` -> 矩形裁切（对齐 petpet 的 `ImageXYWHCropper`）
fn crop_from_values(list: &[Value]) -> Result<Option<Crop>, String> {
    if list.is_empty() {
        return Ok(None);
    }
    let node_at = |index: usize| -> Result<Node, String> {
        Ok(list
            .get(index)
            .map(length::from_json)
            .transpose()?
            .unwrap_or_else(|| Node::px(0.0)))
    };
    let (x1, y1, x2, y2) = match list.len() {
        1 => (Node::px(0.0), Node::px(0.0), node_at(0)?, node_at(0)?),
        2 | 3 => (Node::px(0.0), Node::px(0.0), node_at(0)?, node_at(1)?),
        _ => (node_at(0)?, node_at(1)?, node_at(2)?, node_at(3)?),
    };
    Ok(Some(Crop::Xywh { x1, y1, x2, y2 }))
}

fn parse_radius_list(value: Option<&Value>) -> Vec<RadiusSpec> {
    let Some(value) = value else {
        return Vec::new();
    };
    items_of(Some(value)).into_iter().map(parse_radius).collect()
}

fn parse_radius(value: &Value) -> RadiusSpec {
    if let Some(text) = value.as_str() {
        let parts: Vec<&str> = text.split_whitespace().collect();
        let nodes: Vec<Node> = parts.iter().filter_map(|p| length::parse(p).ok()).collect();
        return match nodes.len() {
            0 => RadiusSpec::None,
            1 => RadiusSpec::Uniform(nodes[0].clone()),
            2 => RadiusSpec::Corners([
                nodes[0].clone(),
                nodes[1].clone(),
                nodes[0].clone(),
                nodes[1].clone(),
            ]),
            _ => RadiusSpec::Corners([
                nodes[0].clone(),
                nodes[1].clone(),
                nodes[2].clone(),
                nodes.get(3).cloned().unwrap_or_else(|| nodes[1].clone()),
            ]),
        };
    }
    match length::from_json(value) {
        Ok(node) => RadiusSpec::Uniform(node),
        Err(_) => RadiusSpec::None,
    }
}

fn parse_filters(value: Option<&Value>) -> Result<Vec<Filter>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in items_of(Some(value)) {
        let Some(kind) = filter_kind(entry) else {
            continue;
        };
        let number = |key: &str, fallback: f32| entry.get(key).and_then(as_f32).unwrap_or(fallback);
        let integer = |key: &str, fallback: i32| {
            entry
                .get(key)
                .and_then(as_f32)
                .map(|value| value.round() as i32)
                .unwrap_or(fallback)
        };
        match kind.to_ascii_lowercase().as_str() {
            "gray" => out.push(Filter::Gray),
            "binarize" | "binarization" | "binary" => out.push(Filter::Binarize),
            "mirror" => out.push(Filter::Mirror),
            "flip" => out.push(Filter::Flip),
            "blur" => out.push(Filter::Blur(number("radius", 10.0))),
            "contrast" => out.push(Filter::Contrast {
                brightness: number("brightness", 0.0),
                contrast: number("contrast", 0.0),
            }),
            "hsb" => out.push(Filter::Hsb {
                hue: number("hue", 0.0),
                saturation: number("saturation", 0.0),
                brightness: number("brightness", 0.0),
            }),
            "noise" => out.push(Filter::Noise(number("amount", 0.25))),
            "swirl" => out.push(Filter::Swirl {
                radius: number("radius", 0.0),
                angle: number("angle", 3.0),
                x: number("x", 0.5),
                y: number("y", 0.5),
            }),
            "bulge" => out.push(Filter::Bulge {
                radius: number("radius", 0.0),
                strength: number("strength", 0.5),
                x: number("x", 0.5),
                y: number("y", 0.5),
            }),
            "swim" => out.push(Filter::Swim {
                scale: number("scale", 32.0),
                stretch: number("stretch", 1.0),
                angle: number("angle", 0.0),
                amount: number("amount", 10.0),
                turbulence: number("turbulence", 1.0),
                time: number("time", 0.0),
            }),
            "halftone" => out.push(Filter::Halftone {
                radius: number("radius", 4.0),
                angle: number("angle", 0.0),
            }),
            "dotscreen" | "dot_screen" => out.push(Filter::DotScreen {
                radius: number("radius", 4.0),
                angle: number("angle", 0.0),
            }),
            "denoise" => out.push(Filter::Denoise),
            "oil" => out.push(Filter::Oil {
                skip: integer("skip", 4),
                range: integer("range", 12),
                levels: integer("levels", 8),
            }),
            "swing_twist" => out.push(Filter::SwingTwist {
                max_angle: number("max_angle", 0.5),
                amplitude: number("amplitude", 0.5),
                omega: number("omega", 2.0 * std::f32::consts::PI / 5.0),
                decay: number("decay", 0.1),
                index_offset: integer("index_offset", 0),
            }),
            "mirage" => {
                let mut keys = string_list(entry.get("key"));
                keys.retain(|key| key != "unknown");
                let mut srcs = string_list(entry.get("default"));
                srcs.extend(string_list(entry.get("src")));
                srcs.retain(|src| !src.is_empty());
                out.push(Filter::Mirage(MirageSpec {
                    keys,
                    srcs,
                    inner_scale: number("inner_scale", 0.3),
                    cover_scale: number("cover_scale", 0.8),
                    inner_desat: number("inner_desat", 0.0),
                    cover_desat: number("cover_desat", 0.0),
                    weight: number("weight", 0.7),
                    max_size: integer("max_size", 1200),
                    colored: entry
                        .get("colored")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                }));
            }
            other => eprintln!("petpet: 暂不支持滤镜 {other}，已忽略"),
        }
    }
    Ok(out)
}

/// 滤镜的 type：支持 `"gray"` 字符串与 `{ "type": "gray", .. }` 两种写法
fn filter_kind(entry: &Value) -> Option<&str> {
    match entry {
        Value::String(name) => Some(name.as_str()),
        other => other.get("type").and_then(Value::as_str),
    }
}
// ==================== 旧版 data.json ====================

fn parse_old(id: &str, dir: &Path, json: &Value) -> Result<Template, String> {
    let kind = match json.get("type").and_then(Value::as_str) {
        Some("GIF") | Some("gif") => Kind::Gif,
        _ => Kind::Image,
    };
    let aliases = string_list(json.get("alias"));
    let in_random_list = json
        .get("inRandomList")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let hidden = json.get("hidden").and_then(Value::as_bool).unwrap_or(false);
    let delay = json.get("delay").and_then(Value::as_i64).unwrap_or(65) as u32;
    let reverse = json.get("reverse").and_then(Value::as_bool).unwrap_or(false);

    let mut elements: Vec<Element> = Vec::new();

    // 旧版背景：模板目录里的 `<数字>.png` 帧序列（petpet 用正则 `\d+.png$` 判断是否存在）；
    // `background` 字段只用来提供画布尺寸 / 颜色 / 帧数，没有帧时不应添加背景元素，
    // 否则会去加载一个不存在的资源。
    let has_background = dir_has_frame_images(dir);
    let canvas = if let Some(bg) = json.get("background") {
        let size = bg.get("size").and_then(Value::as_array);
        let width = size
            .and_then(|s| s.first())
            .map(replace_old_canvas_tokens_value)
            .and_then(|t| length::parse(&t).ok());
        let height = size
            .and_then(|s| s.get(1))
            .map(replace_old_canvas_tokens_value)
            .and_then(|t| length::parse(&t).ok());
        Canvas {
            width,
            height,
            length: bg.get("length").and_then(as_f32).map(Node::px),
            color: bg
                .get("color")
                .and_then(Value::as_str)
                .and_then(parse_color)
                .unwrap_or([255, 255, 255, 0]),
            reverse,
        }
    } else {
        Canvas::default()
    };

    // 绘制顺序对齐 petpet 的 OldPetpetTemplate：非置顶头像 -> 背景 -> 置顶头像 -> 文本
    let mut under: Vec<Element> = Vec::new();
    let mut on_top: Vec<Element> = Vec::new();
    if let Some(list) = json.get("avatar").and_then(Value::as_array) {
        let is_on_top =
            |avatar: &Value| avatar.get("avatarOnTop").and_then(Value::as_bool).unwrap_or(true);
        // 头像 id 按 petpet 的 `avatar{index}` 命名：先非置顶组、后置顶组，各自按数组顺序递增。
        // `background.size` 里的 `avatar0Width` 之类引用就靠这个 id 命中元素尺寸变量。
        let mut avatar_index = 0usize;
        for avatar in list.iter().filter(|avatar| !is_on_top(avatar)) {
            under.push(Element::Image(parse_old_avatar(avatar, avatar_index)?));
            avatar_index += 1;
        }
        for avatar in list.iter().filter(|avatar| is_on_top(avatar)) {
            on_top.push(Element::Image(parse_old_avatar(avatar, avatar_index)?));
            avatar_index += 1;
        }
    }
    elements.extend(under);
    if has_background {
        elements.push(Element::Background(BackgroundElement {
            srcs: vec![dir.to_string_lossy().to_string()],
            reverse,
        }));
    }
    elements.extend(on_top);

    if let Some(list) = json.get("text").and_then(Value::as_array) {
        for text in list {
            elements.push(Element::Text(parse_old_text(text)?));
        }
    }

    let mut template = Template {
        id: id.to_string(),
        dir: dir.to_path_buf(),
        kind,
        canvas,
        elements,
        delays: vec![delay.max(1)],
        aliases,
        description: None,
        in_random_list,
        default_weight: 0,
        hidden,
        preview: None,
        image_keys: BTreeSet::new(),
        text_keys: BTreeSet::new(),
        avatar_sizes: HashMap::new(),
    };
    template.collect_request_keys();
    Ok(template)
}

/// 旧版模板是否使用目录里的 `<数字>.png` 作为背景帧
fn dir_has_frame_images(dir: &Path) -> bool {
    let Ok(reader) = std::fs::read_dir(dir) else {
        return false;
    };
    reader.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.len() > 4
            && name.ends_with(".png")
            && name[..name.len() - 4].chars().all(|ch| ch.is_ascii_digit())
    })
}
fn parse_old_avatar(avatar: &Value, index: usize) -> Result<ImageElement, String> {
    let kind = avatar
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("FROM")
        .to_ascii_uppercase();
    let key = match kind.as_str() {
        "TO" => "to",
        "GROUP" => "group",
        "BOT" => "bot",
        "RANDOM" => "random",
        _ => "from",
    };
    let coords = match avatar.get("pos") {
        Some(pos) => parse_old_coords(pos)?,
        None => vec![default_coords()],
    };

    let mut filters = Vec::new();
    for style in string_list(avatar.get("style")) {
        match style.to_ascii_uppercase().as_str() {
            "MIRROR" => filters.push(Filter::Mirror),
            "FLIP" => filters.push(Filter::Flip),
            "GRAY" => filters.push(Filter::Gray),
            "BINARIZATION" => filters.push(Filter::Binarize),
            _ => {}
        }
    }
    filters.extend(parse_filters(avatar.get("filter"))?);

    let fit = match avatar
        .get("fit")
        .and_then(Value::as_str)
        .unwrap_or("FILL")
        .to_ascii_uppercase()
        .as_str()
    {
        "COVER" => Fit::Cover,
        "CONTAIN" => Fit::Contain,
        "NONE" => Fit::None,
        _ => Fit::Fill,
    };

    Ok(ImageElement {
        id: Some(format!("avatar{index}")),
        keys: vec![key.to_string()],
        srcs: Vec::new(),
        coords,
        crop: parse_old_crop(avatar)?,
        fit,
        position: Offset::center(),
        angle: vec![avatar.get("angle").and_then(as_f32).unwrap_or(0.0)],
        opacity: vec![avatar.get("opacity").and_then(as_f32).unwrap_or(1.0)],
        radius: if avatar.get("round").and_then(Value::as_bool).unwrap_or(false) {
            vec![RadiusSpec::Corners([
                Node::percent(50.0),
                Node::percent(50.0),
                Node::percent(50.0),
                Node::percent(50.0),
            ])]
        } else {
            Vec::new()
        },
        filters,
        origin: match avatar
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or("DEFAULT")
            .to_ascii_uppercase()
            .as_str()
        {
            "CENTER" => Offset::center(),
            _ => length::parse_offset("left top"),
        },
        rotate: if avatar.get("rotate").and_then(Value::as_bool).unwrap_or(false) {
            Some(RotateTransition {
                start: 0.0,
                end: 360.0,
                count: 0,
                easing: Easing::Linear,
            })
        } else {
            None
        },
        start: 0,
        end: -1,
        reverse: false,
    })
}

fn parse_old_coords(pos: &Value) -> Result<Vec<Coords>, String> {
    let list = pos
        .as_array()
        .ok_or_else(|| "旧版 pos 必须是数组".to_string())?;
    let is_point =
        |item: &Value| item.as_array().is_some_and(|pair| pair.len() == 2 && !pair[0].is_array());
    let single = !list.iter().all(Value::is_array)
        || ((list.len() == 4 || list.len() == 5) && list.iter().all(is_point));
    if single {
        return Ok(vec![old_coords_entry(pos)?]);
    }
    let mut out = Vec::new();
    for entry in list {
        out.push(old_coords_entry(entry)?);
    }
    Ok(out)
}

fn old_coords_entry(value: &Value) -> Result<Coords, String> {
    let list = value
        .as_array()
        .ok_or_else(|| "旧版坐标必须是数组".to_string())?;
    let node_at = |index: usize| -> Result<Node, String> {
        match list.get(index) {
            Some(item) => length::parse(&replace_old_element_tokens_value(item))
                .map_err(|err| err.to_string()),
            None => Ok(Node::px(0.0)),
        }
    };
    let is_point =
        |item: &Value| item.as_array().is_some_and(|pair| pair.len() == 2 && !pair[0].is_array());
    if list.len() >= 4 && list.iter().all(is_point) {
        let mut points: Vec<[Node; 2]> = Vec::with_capacity(list.len());
        for point in list {
            let pair = point.as_array().expect("上面已判过是二元点");
            points.push([
                length::parse(&replace_old_element_tokens_value(&pair[0]))?,
                length::parse(&replace_old_element_tokens_value(&pair[1]))?,
            ]);
        }
        let anchor = points.get(4).cloned().unwrap_or_else(|| points[3].clone());
        return Ok(Coords::P4a {
            points: [
                points[0].clone(),
                points[1].clone(),
                points[2].clone(),
                points[3].clone(),
            ],
            anchor,
        });
    }
    Ok(Coords::Xywh {
        x: node_at(0)?,
        y: node_at(1)?,
        w: node_at(2)?,
        h: node_at(3)?,
    })
}

fn parse_old_crop(avatar: &Value) -> Result<Option<Crop>, String> {
    let Some(crop) = avatar.get("crop").and_then(Value::as_array) else {
        return Ok(None);
    };
    let crop_type = avatar
        .get("cropType")
        .and_then(Value::as_str)
        .unwrap_or("NONE");
    if crop_type.eq_ignore_ascii_case("NONE") || crop.is_empty() {
        return Ok(None);
    }
    let percent = crop_type.eq_ignore_ascii_case("PERCENT");
    // 对齐 petpet：PERCENT 用 `cw`/`ch`（相对元素宽/高），不是 `%`
    let axis_unit = |index: usize| -> &'static str {
        if !percent {
            "px"
        } else if index.is_multiple_of(2) {
            "cw"
        } else {
            "ch"
        }
    };
    let value = |index: usize| -> Node {
        crop.get(index)
            .and_then(as_f32)
            .map(|v| {
                length::parse(&format!("{v}{}", axis_unit(index))).unwrap_or_else(|_| Node::px(0.0))
            })
            .unwrap_or_else(|| Node::px(0.0))
    };
    Ok(match crop.len() {
        2 => Some(Crop::Xywh {
            x1: Node::px(0.0),
            y1: Node::px(0.0),
            x2: value(0),
            y2: value(1),
        }),
        4 => Some(Crop::Xywh {
            x1: value(0),
            y1: value(1),
            x2: value(2),
            y2: value(3),
        }),
        _ => None,
    })
}

fn parse_old_text(text: &Value) -> Result<TextElement, String> {
    let raw = text.get("text").and_then(Value::as_str).unwrap_or("");
    let greedy = text.get("greedy").and_then(Value::as_bool).unwrap_or(false);
    let raw = convert_old_text_tokens(raw, greedy);
    let pos = text.get("pos").and_then(Value::as_array);
    let node_at = |index: usize| -> Node {
        pos.and_then(|p| p.get(index))
            .and_then(as_f32)
            .map(Node::px)
            .unwrap_or_else(|| Node::px(0.0))
    };
    // 旧版 position 微调：CENTER -> 50vw/50vh，RIGHT -> 100vw-100cw，BOTTOM -> 100vh-100ch
    let position = string_list(text.get("position"))
        .into_iter()
        .map(|item| item.to_ascii_uppercase())
        .collect::<Vec<_>>();
    let x_axis = position.first().map(String::as_str).unwrap_or("LEFT");
    let y_axis = position.get(1).map(String::as_str).unwrap_or("TOP");
    let x = match x_axis {
        "CENTER" => Node::Add(Box::new(Node::vw(50.0)), Box::new(node_at(0))),
        "RIGHT" => Node::Add(
            Box::new(Node::Sub(
                Box::new(Node::vw(100.0)),
                Box::new(Node::cw(100.0)),
            )),
            Box::new(node_at(0)),
        ),
        _ => node_at(0),
    };
    let bottom = y_axis == "BOTTOM";
    let y = match y_axis {
        "CENTER" => Node::Add(Box::new(Node::vh(50.0)), Box::new(node_at(1))),
        "BOTTOM" => Node::Add(
            Box::new(Node::Sub(
                Box::new(Node::vh(100.0)),
                Box::new(Node::ch(100.0)),
            )),
            Box::new(node_at(1)),
        ),
        _ => node_at(1),
    };
    let coords = TextCoords {
        x,
        y,
        max_w: pos.and_then(|p| p.get(2)).and_then(as_f32).map(Node::px),
    };
    let color = text
        .get("color")
        .and_then(Value::as_str)
        .and_then(parse_color)
        .unwrap_or([25, 25, 25, 255]);
    let wrap = match text
        .get("wrap")
        .and_then(Value::as_str)
        .unwrap_or("NONE")
        .to_ascii_uppercase()
        .as_str()
    {
        "BREAK" => Wrap::Break,
        "ZOOM" => Wrap::Zoom,
        _ => Wrap::None,
    };
    let align = match text
        .get("align")
        .and_then(Value::as_str)
        .unwrap_or("LEFT")
        .to_ascii_uppercase()
        .as_str()
    {
        "CENTER" => Align::Center,
        "RIGHT" => Align::Right,
        _ => Align::Left,
    };
    let baseline = match text
        .get("baseline")
        .and_then(Value::as_str)
        .unwrap_or("ALPHABETIC")
        .to_ascii_uppercase()
        .as_str()
    {
        "MIDDLE" => Baseline::Middle,
        "BOTTOM" => Baseline::Bottom,
        // 旧模板默认 ALPHABETIC
        _ => Baseline::Alphabetic,
    };
    // petpet 旧模板：align=CENTER 时基线强制为 MIDDLE
    let baseline = if align == Align::Center {
        Baseline::Middle
    } else if bottom {
        Baseline::Bottom
    } else {
        baseline
    };
    let stroke_size = text.get("strokeSize").and_then(as_f32).unwrap_or(0.0);
    let stroke_color = text
        .get("strokeColor")
        .and_then(Value::as_str)
        .and_then(parse_color)
        .filter(|_| stroke_size > 0.0);
    let style = match text
        .get("style")
        .and_then(Value::as_str)
        .unwrap_or("PLAIN")
        .to_ascii_uppercase()
        .as_str()
    {
        "BOLD" => TextStyleKind::Bold,
        "ITALIC" => TextStyleKind::Italic,
        "BOLD_ITALIC" => TextStyleKind::BoldItalic,
        _ => TextStyleKind::Plain,
    };
    // 旧模板 ZOOM：size 是最大字号，从 16 开始按宽度放大（petpet 只放大不缩小）
    let base_size = text.get("size").and_then(as_f32).unwrap_or(16.0);
    let (min_size, max_size) = if wrap == Wrap::Zoom {
        (16.0f32, base_size)
    } else {
        (base_size, 96.0f32)
    };
    Ok(TextElement {
        id: None,
        text: vec![raw],
        coords: vec![coords],
        color: vec![color],
        font: string_list(text.get("font")),
        size: vec![min_size],
        max_size: vec![max_size],
        align,
        baseline,
        wrap,
        angle: vec![text.get("angle").and_then(as_f32).unwrap_or(0.0)],
        opacity: vec![1.0],
        stroke_color,
        stroke_size,
        style,
        origin: match text
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or("DEFAULT")
            .to_ascii_uppercase()
            .as_str()
        {
            "CENTER" => Offset::center(),
            _ => length::parse_offset("left top"),
        },
        start: text.get("start").and_then(Value::as_i64).unwrap_or(0),
        end: text.get("end").and_then(Value::as_i64).unwrap_or(-1),
    })
}

/// 旧版 `background.size`：只把结尾的 `Width` / `Height` 换成元素尺寸变量后缀
/// （`avatar0Width*2` → `avatar0_width*2`），不能碰小写的 `width` / `height`。
fn replace_old_canvas_tokens_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.replace("Width", "_width").replace("Height", "_height"),
        Value::Number(number) => number.to_string(),
        other => format!("{other}"),
    }
}

/// 旧版 `pos`：小写 `width` / `height` 指画布尺寸，对齐 petpet 的 `100cw` / `100ch`。
fn replace_old_element_tokens_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.replace("width", "100cw").replace("height", "100ch"),
        Value::Number(number) => number.to_string(),
        other => format!("{other}"),
    }
}

/// 旧版 `$txtN[默认值]` → 新版 `${N-1:默认值}`；`$from` → `${from:-from}`。
///
/// `greedy` 对齐 petpet 的 `OldTextTemplate.greedy`：第一个 `$txtN[...]` 会被换成 `${raw:...}`，
/// 也就是"整条消息原文"而不是按空格切的第 N 个词。
fn convert_old_text_tokens(text: &str, greedy: bool) -> String {
    let mut result = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut should_replace_raw = greedy;
    while i < chars.len() {
        if chars[i] == '$'
            && chars.get(i + 1) == Some(&'t')
            && chars.get(i + 2) == Some(&'x')
            && chars.get(i + 3) == Some(&'t')
        {
            let start = i + 4;
            let mut j = start;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            // petpet 的正则是 `\$txt([1-9]\d*)\[(.*?)]`：索引从 1 开始、默认值非贪婪到第一个 `]`
            if j > start && chars[start] != '0' && chars.get(j) == Some(&'[') {
                let index: usize = chars[start..j]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .unwrap_or(1);
                let mut k = j + 1;
                while k < chars.len() && chars[k] != ']' {
                    k += 1;
                }
                if k < chars.len() {
                    let default: String = chars[j + 1..k].iter().collect();
                    let key = if should_replace_raw {
                        should_replace_raw = false;
                        "raw".to_string()
                    } else {
                        index.saturating_sub(1).to_string()
                    };
                    result.push_str(&format!("${{{key}:{default}}}"));
                    i = k + 1;
                    continue;
                }
            }
        }
        result.push(chars[i]);
        i += 1;
    }
    for token in ["from", "to", "group", "bot"] {
        result = result.replace(&format!("${token}"), &format!("${{{token}:-{token}}}"));
    }
    result
}
// ==================== 请求键与工具 ====================

impl Template {
    fn collect_request_keys(&mut self) {
        for element in &self.elements {
            match element {
                Element::Image(image) => {
                    let mut size = 0u32;
                    for coords in &image.coords {
                        let (w, h) = coords_size(coords);
                        size = size.max(w.max(h).round() as u32);
                    }
                    for key in &image.keys {
                        if key == "unknown" {
                            continue;
                        }
                        self.image_keys.insert(key.clone());
                        let entry = self.avatar_sizes.entry(key.clone()).or_insert(0);
                        *entry = (*entry).max(size);
                    }
                }
                Element::Text(text) => {
                    for raw in &text.text {
                        for name in placeholders(raw) {
                            self.text_keys.insert(name);
                        }
                    }
                }
                Element::Background(_) => {}
            }
        }
        for key in ["from", "to", "group", "bot"] {
            self.text_keys.insert(key.to_string());
        }
    }

    /// 模板索引里的一行：`id  ( alias, alias )`
    pub fn index_line(&self) -> String {
        if self.aliases.is_empty() {
            self.id.clone()
        } else {
            format!("{}  ( {} )", self.id, self.aliases.join(", "))
        }
    }
}

/// 从 `${name}` / `${name:-default}` 里取出 name
pub fn placeholders(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == '$' && chars[i + 1] == '{' {
            let mut j = i + 2;
            let mut inner = String::new();
            while j < chars.len() && chars[j] != '}' {
                inner.push(chars[j]);
                j += 1;
            }
            let name = inner.split(":-").next().unwrap_or(&inner).trim();
            if !name.is_empty() {
                out.push(name.to_string());
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// 把"单值或数组"统一成元素列表
pub fn items(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(list) => list.iter().collect(),
        other => vec![other],
    }
}

fn items_of(value: Option<&Value>) -> Vec<&Value> {
    value.map(items).unwrap_or_default()
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    items_of(value)
        .into_iter()
        .filter_map(|v| v.as_str())
        .map(str::to_string)
        .collect()
}

fn float_list(value: Option<&Value>, fallback: f32) -> Vec<f32> {
    let mut out: Vec<f32> = items_of(value).into_iter().filter_map(as_f32).collect();
    if out.is_empty() {
        out.push(fallback);
    }
    out
}

fn as_f32(value: &Value) -> Option<f32> {
    value
        .as_f64()
        .map(|v| v as f32)
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
}

/// 解析 `#rgb` / `#rgba` / `#rrggbb` / `#rrggbbaa`，缺省 alpha 为不透明
pub fn parse_color(text: &str) -> Option<[u8; 4]> {
    let hex = text.trim().trim_start_matches('#');
    let digits: Vec<u8> = hex
        .chars()
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect::<Option<Vec<u8>>>()?;
    let pair = |hi: u8, lo: u8| hi * 16 + lo;
    match digits.len() {
        3 => Some([digits[0] * 17, digits[1] * 17, digits[2] * 17, 255]),
        4 => Some([
            digits[0] * 17,
            digits[1] * 17,
            digits[2] * 17,
            digits[3] * 17,
        ]),
        6 => Some([
            pair(digits[0], digits[1]),
            pair(digits[2], digits[3]),
            pair(digits[4], digits[5]),
            255,
        ]),
        8 => Some([
            pair(digits[0], digits[1]),
            pair(digits[2], digits[3]),
            pair(digits[4], digits[5]),
            pair(digits[6], digits[7]),
        ]),
        _ => None,
    }
}

/// 按帧取动画数组里的值（越界时取最后一帧）
pub fn pick<T>(list: &[T], frame: usize) -> Option<&T> {
    if list.is_empty() {
        None
    } else {
        list.get(frame.min(list.len() - 1)).or_else(|| list.last())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_new_template() {
        let json: Value = serde_json::from_str(
            r#"{
              "type": "image",
              "metadata": {"alias": ["a", "b"], "description": "d"},
              "elements": [
                {"type": "image", "key": "to", "coords": [0, 0, 100, 100]},
                {"type": "text", "coords": [0, 0], "text": "hi ${from}"}
              ],
              "canvas": {"width": 200, "height": 100}
            }"#,
        )
        .unwrap();
        let template = parse_new("demo", Path::new("."), &json).unwrap();
        assert_eq!(template.kind, Kind::Image);
        assert!(template.image_keys.contains("to"));
        assert!(template.text_keys.contains("from"));
        assert_eq!(template.avatar_sizes.get("to"), Some(&100));
        assert_eq!(template.aliases, vec!["a", "b"]);
    }

    #[test]
    fn parses_old_template() {
        let json: Value = serde_json::from_str(
            r#"{
              "type": "IMG",
              "avatar": [{"type": "TO", "pos": [0, 0, 100, 100]}],
              "text": [{"text": "$txt1[default]", "pos": [0, 0]}],
              "alias": ["x"],
              "delay": 80
            }"#,
        )
        .unwrap();
        let template = parse_old("old", Path::new("."), &json).unwrap();
        assert_eq!(template.kind, Kind::Image);
        assert_eq!(template.delays, vec![80]);
        assert!(template.image_keys.contains("to"));
    }

    #[test]
    fn old_template_uses_dir_frames_as_background() {
        let dir = std::env::temp_dir().join(format!("petpet_old_tpl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 目录里的 `<数字>.png` 就是旧模板的隐式背景帧
        image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]))
            .save(dir.join("0.png"))
            .unwrap();

        let json: Value = serde_json::from_str(
            r#"{
              "type": "GIF",
              "avatar": [
                {"type": "FROM", "pos": [0, 0, 10, 10], "avatarOnTop": false},
                {"type": "TO", "pos": [0, 0, 10, 10]}
              ],
              "text": [{"text": "$txt1[默认]", "pos": [0, 0], "size": 40, "wrap": "ZOOM", "align": "CENTER"}]
            }"#,
        )
        .unwrap();
        let template = parse_old("old", &dir, &json).unwrap();

        // 顺序：非置顶头像 -> 背景 -> 置顶头像 -> 文本（与 petpet 的 OldPetpetTemplate 一致）
        assert!(matches!(template.elements[0], Element::Image(_)));
        assert!(matches!(template.elements[1], Element::Background(_)));
        assert!(matches!(template.elements[2], Element::Image(_)));
        assert!(matches!(template.elements[3], Element::Text(_)));

        let Element::Text(text) = &template.elements[3] else {
            unreachable!()
        };
        assert_eq!(text.text[0], "${0:默认}");
        // ZOOM：size 是放大基准，max_size 才是模板里写的字号
        assert_eq!(text.size[0], 16.0);
        assert_eq!(text.max_size[0], 40.0);
        // align=CENTER 时基线强制为 MIDDLE
        assert_eq!(text.align, Align::Center);
        assert_eq!(text.baseline, Baseline::Middle);
    }
    #[test]
    fn converts_old_text_tokens() {
        assert_eq!(convert_old_text_tokens("$txt1[x]", false), "${0:x}");
        assert_eq!(convert_old_text_tokens("hi $from", false), "hi ${from:-from}");
        // greedy：第一个 $txtN[] 变成整条原文
        assert_eq!(
            convert_old_text_tokens("$txt1[a] $txt2[b]", true),
            "${raw:a} ${1:b}"
        );
        assert_eq!(convert_old_text_tokens("$txt2[b]", true), "${raw:b}");
        assert_eq!(convert_old_text_tokens("$txt0[x]", false), "$txt0[x]");
    }

    #[test]
    fn parses_colors() {
        assert_eq!(parse_color("#fff"), Some([255, 255, 255, 255]));
        assert_eq!(parse_color("#00000000"), Some([0, 0, 0, 0]));
        assert_eq!(parse_color("#ff000080"), Some([255, 0, 0, 128]));
        assert_eq!(parse_color("12345"), None);
        assert_eq!(parse_color("zzz"), None);
    }
}

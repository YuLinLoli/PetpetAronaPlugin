//! 文本排版与栅格化（ab_glyph 惰性后端）。移植 petpet 文本元素的常用子集：
//! `${var}` 变量替换在调用方完成；这里负责换行、对齐、基线、描边与逐字绘制。
//!
//! 字面优先要框架随包的那一份（`arona::runtime::assets::cjk_font()`，安装目录 `fonts/` 下的
//! 思源宋体简中），拿不到才退回系统字体目录那串候选，配置里指定的路径再覆盖两者。
//!
//! 栅格化是惰性的：内存里只留字体文件的原始字节，画到哪个字才解析哪个字，栅格结果进一层
//! 按 (字符, 字号) 计的有界 LRU。换掉的是 fontdue 那种"把每个码位的轮廓一次全解析完"的路子
//! （那一档实测常驻 160.6 MB、解析 0.33 s；这一档是文件字节那一档 + 几 MB 缓存、装载 0.04 s）。
//! 两档后端的几何在 bilibili 插件那侧逐字比过（TrueType 与 CFF 两类轮廓都验），包围盒与 advance
//! 最大偏差 0 格、0.0 px——换的不是观感，只是谁在什么时候解析轮廓。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ab_glyph::{Font as _, Glyph, ScaleFont};
use image::{Rgba, RgbaImage};

use crate::template::{Align, Baseline, TextStyleKind, Wrap};

/// 字形缓存上限。一条 petpet 动图一帧里用到的 (字符, 字号) 就几十到几百个，4096 条按 1~2 KB 算
/// 是几 MB 量级；到顶丢最久没被用的那条，所以"文案里出现过的所有字"不会一直堆着
const GLYPH_CACHE_MAX: usize = 4096;

/// 一个栅格好的字形：落点 + 覆盖率位图。落点口径沿用 fontdue 的 `Metrics`——`xmin`/`ymin`
/// 是相对笔位置（水平笔位、垂直基线）的左边界与**下**边界，y 向上为正，下面那一路照旧用
struct Raster {
    xmin: i32,
    ymin: i32,
    width: usize,
    height: usize,
    advance: f32,
    coverage: Vec<u8>,
}

type Cached = std::sync::Arc<Raster>;

/// 有界的字形 LRU，口径与 bilibili、BlueArchive 两份一致：命中挪到队尾，到顶从头上摘
struct Cache {
    map: HashMap<(char, u32), Cached>,
    order: VecDeque<(char, u32)>,
}

impl Cache {
    fn touch(&mut self, key: (char, u32)) {
        if let Some(at) = self.order.iter().position(|have| *have == key) {
            self.order.remove(at);
        }
        self.order.push_back(key);
    }

    fn put(&mut self, key: (char, u32), done: Cached) {
        while self.map.len() >= GLYPH_CACHE_MAX && self.map.len() > 1 {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.map.remove(&oldest);
                }
                None => break,
            }
        }
        self.map.insert(key, done);
        self.touch(key);
    }
}

/// 已加载的字体
pub struct FontSet {
    font: ab_glyph::FontArc,
    glyphs: Mutex<Cache>,
    /// 实际使用的字体路径（日志用）
    pub source: String,
}

impl FontSet {
    pub fn load(path: &Path) -> Result<FontSet, String> {
        let data = std::fs::read(path).map_err(|err| format!("读取字体失败: {err}"))?;
        let font = ab_glyph::FontArc::try_from_vec(data)
            .map_err(|err| format!("解析字体 {} 失败: {err}", path.display()))?;
        Ok(FontSet {
            font,
            glyphs: Mutex::new(Cache {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
            source: path.display().to_string(),
        })
    }

    /// 按给定路径 → 框架随包那份 → 系统字体目录的顺序找一份能显示中文的字体
    pub fn detect(configured: &str) -> Option<FontSet> {
        let configured = configured.trim();
        if !configured.is_empty() {
            match FontSet::load(Path::new(configured)) {
                Ok(font) => return Some(font),
                Err(err) => eprintln!("petpet: {err}"),
            }
        }
        if let Some((regular, _)) = arona::runtime::assets::cjk_font() {
            match FontSet::load(&regular) {
                Ok(font) => return Some(font),
                Err(err) => eprintln!("petpet: {err}"),
            }
        }
        for path in system_font_candidates() {
            if path.is_file()
                && let Ok(font) = FontSet::load(&path)
            {
                return Some(font);
            }
        }
        None
    }

    /// 字号(px，即 em 的像素数) → ab_glyph 要的 `PxScale`。
    ///
    /// `PxScale` 的基准不是 em 而是 `height_unscaled() = ascent - descent`（雅黑那一档 1320
    /// 而 unitsPerEm 是 1000），不换算所有文字会整体小 24%、模板里量出来的坐标全乱。
    fn scale(&self, px: f32) -> f32 {
        let em = self.font.units_per_em().unwrap_or(1000.0);
        if em == 0.0 {
            return px;
        }
        px * self.font.height_unscaled() / em
    }

    /// 与 fontdue 的 `horizontal_line_metrics` 同式：hhea 那三个数是字库单位，乘 `px / units_per_em`
    /// 落到像素，`new_line_size = ascent - descent + line_gap`。取不到就按 0.8/-0.2/1.2 那一档兜底。
    fn line_metrics(&self, size: f32) -> (f32, f32, f32) {
        match self.font.units_per_em() {
            Some(em) if em != 0.0 => {
                let k = size / em;
                let ascent = self.font.ascent_unscaled() * k;
                let descent = self.font.descent_unscaled() * k;
                let line_gap = self.font.line_gap_unscaled() * k;
                (ascent, descent, ascent - descent + line_gap)
            }
            _ => (size * 0.8, -size * 0.2, size * 1.2),
        }
    }

    pub fn advance(&self, ch: char, size: f32) -> f32 {
        let scaled = self.font.as_scaled(self.scale(size));
        scaled.h_advance(scaled.glyph_id(ch))
    }

    /// 取一个字的栅格：命中缓存只加一次引用，没命中才解析轮廓
    fn raster(&self, ch: char, size: f32) -> Cached {
        let key = (ch, size.to_bits());
        let mut cache = self
            .glyphs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(hit) = cache.map.get(&key) {
            let hit = Cached::clone(hit);
            cache.touch(key);
            return hit;
        }
        let done = std::sync::Arc::new(uncached(&self.font, self.scale(size), ch));
        cache.put(key, Cached::clone(&done));
        done
    }

    pub fn text_width(&self, text: &str, size: f32) -> f32 {
        text.chars().map(|ch| self.advance(ch, size)).sum::<f32>()
    }

    /// 按最大宽度折行（wrap=break/zoom 用）
    pub fn wrap_lines(&self, text: &str, size: f32, max_width: f32) -> Vec<String> {
        let mut lines = Vec::new();
        for raw in text.split('\n') {
            if raw.is_empty() {
                lines.push(String::new());
                continue;
            }
            let mut current = String::new();
            let mut current_width = 0.0f32;
            for ch in raw.chars() {
                let advance = self.advance(ch, size);
                if !current.is_empty() && current_width + advance > max_width {
                    lines.push(std::mem::take(&mut current));
                    current_width = 0.0;
                }
                current.push(ch);
                current_width += advance;
            }
            if !current.is_empty() {
                lines.push(current);
            }
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        lines
    }
}

/// 真正解析一个字的轮廓并铺成覆盖率位图（缓存没命中时才走这里）
fn uncached(font: &ab_glyph::FontArc, scale: f32, ch: char) -> Raster {
    let scaled = font.as_scaled(scale);
    let id = scaled.glyph_id(ch);
    let advance = scaled.h_advance(id);
    let glyph: Glyph = id.with_scale(scale);
    let Some(outline) = font.outline_glyph(glyph) else {
        return Raster {
            xmin: 0,
            ymin: 0,
            width: 0,
            height: 0,
            advance,
            coverage: Vec::new(),
        };
    };
    // ab_glyph 的 `px_bounds` 是 **y 向下**的像素包围盒（min.y 是顶边），而下面那一路沿用 fontdue
    // 的口径：`ymin` 是相对基线的**下**边界、向上为正。换算就一次取负。
    let bounds = outline.px_bounds();
    let (left, right) = (bounds.min.x.floor(), bounds.max.x.ceil());
    let (top, bottom) = (bounds.min.y.floor(), bounds.max.y.ceil());
    let width = (right - left).max(0.0) as usize;
    let height = (bottom - top).max(0.0) as usize;
    let mut coverage = vec![0u8; width * height];
    if width > 0 && height > 0 {
        outline.draw(|x, y, value| {
            let (x, y) = (x as usize, y as usize);
            if x < width && y < height {
                coverage[y * width + x] = (value * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        });
    }
    Raster {
        xmin: left as i32,
        ymin: -(bottom as i32),
        width,
        height,
        advance,
        coverage,
    }
}

fn system_font_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(windir) = std::env::var("WINDIR") {
        let fonts = Path::new(&windir).join("Fonts");
        for name in [
            "msyh.ttc",
            "msyhbd.ttc",
            "simhei.ttf",
            "simsun.ttc",
            "Deng.ttf",
            "simkai.ttf",
        ] {
            paths.push(fonts.join(name));
        }
    }
    #[cfg(not(windows))]
    {
        for name in [
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/System/Library/Fonts/PingFang.ttc",
        ] {
            paths.push(PathBuf::from(name));
        }
    }
    paths
}

/// 一次文本绘制需要的样式（尺寸/颜色等已按帧取好）
pub struct TextStyle {
    pub size: f32,
    pub max_size: f32,
    pub color: [u8; 4],
    pub align: Align,
    pub baseline: Baseline,
    pub wrap: Wrap,
    pub max_width: Option<f32>,
    pub stroke_color: Option<[u8; 4]>,
    pub stroke_size: f32,
    pub opacity: f32,
    /// 粗体/斜体（由渲染层合成）
    pub style: TextStyleKind,
}

/// 排版结果：绘制到一张紧贴文字的 RGBA 图上，并给出锚点在图内的位置
pub struct TextBitmap {
    pub image: RgbaImage,
    pub anchor_x: f32,
    pub anchor_y: f32,
}

pub fn render_text(font: &FontSet, content: &str, style: &TextStyle) -> TextBitmap {
    let bold = matches!(style.style, TextStyleKind::Bold | TextStyleKind::BoldItalic);
    let slant = if matches!(
        style.style,
        TextStyleKind::Italic | TextStyleKind::BoldItalic
    ) {
        0.25
    } else {
        0.0
    };
    let (mut size, lines) = layout(font, content, style);
    if size < 1.0 {
        size = 1.0;
    }
    let (ascent, descent, line_height) = {
        let (a, d, l) = font.line_metrics(size);
        (a, d, l)
    };
    let _ = descent;

    let width = lines
        .iter()
        .map(|line| font.text_width(line, size))
        .fold(0.0f32, f32::max);
    let height = line_height * lines.len() as f32;
    let pad = if style.stroke_color.is_some() {
        style.stroke_size.ceil().max(0.0)
    } else {
        0.0
    };
    let image_w = (width + pad * 2.0).ceil().max(1.0) as u32;
    let image_h = (height + pad * 2.0).ceil().max(1.0) as u32;
    let mut image = RgbaImage::new(image_w, image_h);

    for (line_index, line) in lines.iter().enumerate() {
        let baseline = pad + ascent + line_index as f32 * line_height;
        let mut pen_x = pad;
        for ch in line.chars() {
            let metrics = font.raster(ch, size);
            let glyph_x = pen_x + metrics.xmin as f32;
            let glyph_y = baseline - (metrics.height as f32 + metrics.ymin as f32);
            if metrics.width > 0 && metrics.height > 0 {
                if let Some(stroke) = style.stroke_color {
                    draw_glyph(
                        &mut image,
                        &metrics.coverage,
                        metrics.width,
                        metrics.height,
                        glyph_x,
                        glyph_y,
                        stroke,
                        style.opacity,
                        style.stroke_size,
                        false,
                        0.0,
                    );
                }
                draw_glyph(
                    &mut image,
                    &metrics.coverage,
                    metrics.width,
                    metrics.height,
                    glyph_x,
                    glyph_y,
                    style.color,
                    style.opacity,
                    0.0,
                    bold,
                    slant,
                );
            }
            pen_x += metrics.advance;
        }
    }

    let anchor_x = match style.align {
        Align::Left => 0.0,
        Align::Center => image_w as f32 / 2.0,
        Align::Right => image_w as f32,
    };
    let anchor_y = match style.baseline {
        Baseline::Top => 0.0,
        Baseline::Alphabetic => pad + ascent,
        Baseline::Middle => image_h as f32 / 2.0,
        Baseline::Bottom => image_h as f32,
    };
    TextBitmap {
        image,
        anchor_x,
        anchor_y,
    }
}

/// 计算最终字号与折行
fn layout(font: &FontSet, content: &str, style: &TextStyle) -> (f32, Vec<String>) {
    let max_width = style.max_width.filter(|w| *w > 0.0);
    match style.wrap {
        Wrap::None => (style.size, split_lines(content)),
        Wrap::Break => match max_width {
            Some(max_width) => (style.size, font.wrap_lines(content, style.size, max_width)),
            None => (style.size, split_lines(content)),
        },
        Wrap::Zoom => {
            // 与 petpet GraphicsZoomParagraph 一致：以 size 为基准按宽度放大，只放大不缩小，
            // 上限是 max_size；ZOOM 不折行，只按 `\n` 分行。
            let Some(max_width) = max_width else {
                return (style.size, split_lines(content));
            };
            let lines = split_lines(content);
            let widest = lines
                .iter()
                .map(|line| font.text_width(line, style.size))
                .fold(0.0f32, f32::max);
            if widest <= 0.0 {
                return (style.size, lines);
            }
            let multiple = (max_width / widest).max(1.0);
            let size = (style.size * multiple)
                .round()
                .clamp(1.0, style.max_size.max(1.0));
            (size, lines)
        }
    }
}

fn split_lines(content: &str) -> Vec<String> {
    let lines: Vec<String> = content.split('\n').map(str::to_string).collect();
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_glyph(
    image: &mut RgbaImage,
    bitmap: &[u8],
    width: usize,
    height: usize,
    x: f32,
    y: f32,
    color: [u8; 4],
    opacity: f32,
    spread: f32,
    bold: bool,
    slant: f32,
) {
    let offsets: &[(f32, f32)] = if spread > 0.0 {
        &[
            (0.0, 0.0),
            (1.0, 0.0),
            (-1.0, 0.0),
            (0.0, 1.0),
            (0.0, -1.0),
            (1.0, 1.0),
            (-1.0, 1.0),
            (1.0, -1.0),
            (-1.0, -1.0),
        ]
    } else if bold {
        // 用四个亚像素偏移叠画模拟加粗
        &[(0.0, 0.0), (0.6, 0.0), (0.0, 0.6), (0.6, 0.6)]
    } else {
        &[(0.0, 0.0)]
    };
    let scale = if spread <= 0.0 { 1.0 } else { spread.max(1.0) };
    for (ox, oy) in offsets {
        for row in 0..height {
            for col in 0..width {
                let coverage = bitmap[row * width + col];
                if coverage == 0 {
                    continue;
                }
                let slope = ((height as f32 - 1.0 - row as f32) * slant).round();
                let px = (x + *ox * scale + slope).round() as i32 + col as i32;
                let py = (y + *oy * scale).round() as i32 + row as i32;
                blend(image, px, py, color, coverage, opacity);
            }
        }
    }
}

/// 把带覆盖率的颜色混到目标像素上
pub fn blend(image: &mut RgbaImage, x: i32, y: i32, color: [u8; 4], coverage: u8, opacity: f32) {
    if x < 0 || y < 0 || x >= image.width() as i32 || y >= image.height() as i32 {
        return;
    }
    let alpha = (coverage as f32 / 255.0) * (color[3] as f32 / 255.0) * opacity.clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return;
    }
    let dst = image.get_pixel_mut(x as u32, y as u32);
    let out_r = color[0] as f32 * alpha + dst[0] as f32 * (1.0 - alpha);
    let out_g = color[1] as f32 * alpha + dst[1] as f32 * (1.0 - alpha);
    let out_b = color[2] as f32 * alpha + dst[2] as f32 * (1.0 - alpha);
    let out_a = (alpha + dst[3] as f32 / 255.0 * (1.0 - alpha)).min(1.0);
    *dst = Rgba([
        out_r.clamp(0.0, 255.0) as u8,
        out_g.clamp(0.0, 255.0) as u8,
        out_b.clamp(0.0, 255.0) as u8,
        (out_a * 255.0).clamp(0.0, 255.0) as u8,
    ]);
}

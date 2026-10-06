//! 文本排版与栅格化（fontdue）。移植 petpet 文本元素的常用子集：
//! `${var}` 变量替换在调用方完成；这里负责换行、对齐、基线、描边与逐字绘制。

use std::path::{Path, PathBuf};

use fontdue::{Font, FontSettings};
use image::{Rgba, RgbaImage};

use crate::template::{Align, Baseline, TextStyleKind, Wrap};

/// 已加载的字体
pub struct FontSet {
    font: Font,
    /// 实际使用的字体路径（日志用）
    pub source: String,
}

impl FontSet {
    pub fn load(path: &Path) -> Result<FontSet, String> {
        let data = std::fs::read(path).map_err(|err| format!("读取字体失败: {err}"))?;
        let settings = FontSettings {
            collection_index: 0,
            ..Default::default()
        };
        let font = Font::from_bytes(data, settings)
            .map_err(|err| format!("解析字体 {} 失败: {err}", path.display()))?;
        Ok(FontSet {
            font,
            source: path.display().to_string(),
        })
    }

    /// 按给定路径或系统字体目录寻找一份能显示中文的字体
    pub fn detect(configured: &str) -> Option<FontSet> {
        if !configured.trim().is_empty() {
            match FontSet::load(Path::new(configured.trim())) {
                Ok(font) => return Some(font),
                Err(err) => eprintln!("petpet: {err}"),
            }
        }
        for path in system_font_candidates() {
            if path.is_file()
                && let Ok(font) = FontSet::load(&path) {
                    return Some(font);
                }
        }
        None
    }

    fn line_metrics(&self, size: f32) -> (f32, f32, f32) {
        match self.font.horizontal_line_metrics(size) {
            Some(metrics) => (metrics.ascent, metrics.descent, metrics.new_line_size),
            None => (size * 0.8, -size * 0.2, size * 1.2),
        }
    }

    pub fn advance(&self, ch: char, size: f32) -> f32 {
        self.font.metrics(ch, size).advance_width
    }

    pub fn text_width(&self, text: &str, size: f32) -> f32 {
        text.chars()
            .map(|ch| self.advance(ch, size))
            .sum::<f32>()
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
    let slant = if matches!(style.style, TextStyleKind::Italic | TextStyleKind::BoldItalic) {
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
            let (metrics, bitmap) = font.font.rasterize(ch, size);
            let glyph_x = pen_x + metrics.xmin as f32;
            let glyph_y = baseline - (metrics.height as f32 + metrics.ymin as f32);
            if metrics.width > 0 && metrics.height > 0 {
                if let Some(stroke) = style.stroke_color {
                    draw_glyph(
                        &mut image,
                        &bitmap,
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
                    &bitmap,
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
            pen_x += metrics.advance_width;
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
            Some(max_width) => (
                style.size,
                font.wrap_lines(content, style.size, max_width),
            ),
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
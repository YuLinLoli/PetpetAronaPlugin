//! 文本排版与绘制。移植 petpet 文本元素的常用子集：
//! `${var}` 变量替换在调用方完成；这里负责换行、对齐、基线、描边与逐字绘制。
//!
//! 字面那一层在框架的 `arona::runtime::text` 里：三级解析（配置 / 随包 `fonts/` 的思源宋体 /
//! 系统候选）、按 (字符, 字号) 计的有界字形缓存、`PxScale` 换算、Weak 缓存，全在那一处，
//! 三个出图插件共用。本模块只做 petpet 模板要的：折行、对齐、描边、斜体与真粗体。
//!
//! **粗体用的是随包那份真 Bold**（`SourceHanSerifCN-Bold.otf`），不是把常规描粗：
//! 四亚像素偏移叠画那种假粗在衬线体上会把笔画糊掉。字面里没配到粗体那一档时才退回叠画，
//! 那种情况下没有更好的选择（用户把自己那份单文件放进 `fonts/` 时就是这一档）。

use arona::runtime::text::{self as font, Face};
use image::{Rgba, RgbaImage};
use std::sync::Arc;

use crate::template::{Align, Baseline, TextStyleKind, Wrap};

/// 一次出图要用的字面：框架那份 `Typeface`（常规 + 可选粗体）加上"用的是哪个文件"这行日志要的话
pub struct FontSet {
    face: Arc<font::Typeface>,
    /// 实际使用的字体路径（日志用）
    pub source: String,
}

impl FontSet {
    /// 按三级解析取字面：配置里指定的路径 > 框架随包那份（安装目录 `fonts/`）> 系统候选。
    /// 一份都没有就回 `None`，由调用方按"没有可渲染中文的字体"办。
    pub fn detect(configured: &str) -> Option<FontSet> {
        let face = font::Typeface::discover(configured, &[]).ok()?;
        let source = face.paths().0.display().to_string();
        Some(FontSet { face, source })
    }

    /// 有没有真的粗体那一档。没有的话 `bold` 样式只能靠叠画模拟（见 [`render_text`]）
    pub fn has_bold(&self) -> bool {
        self.face.has_bold()
    }

    /// 这一档样式该用哪一面：要粗体且字面里真有粗体才换面，否则仍是常规
    fn face(&self, bold: bool) -> &Face {
        self.face.face(bold && self.has_bold())
    }

    /// 单行基线指标。口径由框架的 `Face::line_metrics` 保证与 fontdue 一致；
    /// 字库没给 hhea 那一档时按 0.8/-0.2/1.2 兜底（与换后端之前同一档）。
    fn line_metrics(&self, size: f32, bold: bool) -> (f32, f32, f32) {
        match self.face(bold).line_metrics(size) {
            Some(metrics) => (metrics.ascent, metrics.descent, metrics.new_line_size),
            None => (size * 0.8, -size * 0.2, size * 1.2),
        }
    }

    pub fn advance(&self, ch: char, size: f32, bold: bool) -> f32 {
        self.face(bold).advance(ch, size)
    }

    pub fn text_width(&self, text: &str, size: f32, bold: bool) -> f32 {
        self.face(bold).text_width(text, size)
    }

    /// 按最大宽度折行（wrap=break/zoom 用）
    pub fn wrap_lines(&self, text: &str, size: f32, max_width: f32, bold: bool) -> Vec<String> {
        let mut lines = Vec::new();
        for raw in text.split('\n') {
            if raw.is_empty() {
                lines.push(String::new());
                continue;
            }
            let mut current = String::new();
            let mut current_width = 0.0f32;
            for ch in raw.chars() {
                let advance = self.advance(ch, size, bold);
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
    // 有真粗体就换面，不再叠画；没有才退回四亚像素偏移那档模拟
    let synthesized = bold && !font.has_bold();
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
    let (ascent, _descent, line_height) = font.line_metrics(size, bold);

    let width = lines
        .iter()
        .map(|line| font.text_width(line, size, bold))
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
    let face = font.face(bold);

    for (line_index, line) in lines.iter().enumerate() {
        let baseline = pad + ascent + line_index as f32 * line_height;
        let mut pen_x = pad;
        for ch in line.chars() {
            let (metrics, coverage) = face.rasterize(ch, size);
            let glyph_x = pen_x + metrics.xmin as f32;
            let glyph_y = baseline - (metrics.height as f32 + metrics.ymin as f32);
            if metrics.width > 0 && metrics.height > 0 {
                if let Some(stroke) = style.stroke_color {
                    draw_glyph(
                        &mut image,
                        &coverage,
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
                    &coverage,
                    metrics.width,
                    metrics.height,
                    glyph_x,
                    glyph_y,
                    style.color,
                    style.opacity,
                    0.0,
                    synthesized,
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

/// 计算最终字号与折行。量的时候用哪一面，画的时候就得用哪一面——粗体比常规宽，
/// 拿常规去量、用粗体去画会溢出模板里那个框。
fn layout(font: &FontSet, content: &str, style: &TextStyle) -> (f32, Vec<String>) {
    let bold = matches!(style.style, TextStyleKind::Bold | TextStyleKind::BoldItalic);
    let max_width = style.max_width.filter(|w| *w > 0.0);
    match style.wrap {
        Wrap::None => (style.size, split_lines(content)),
        Wrap::Break => match max_width {
            Some(max_width) => (
                style.size,
                font.wrap_lines(content, style.size, max_width, bold),
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
                .map(|line| font.text_width(line, style.size, bold))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn style(kind: TextStyleKind) -> TextStyle {
        TextStyle {
            size: 40.0,
            max_size: 60.0,
            color: [0, 0, 0, 255],
            align: Align::Left,
            baseline: Baseline::Top,
            wrap: Wrap::None,
            max_width: None,
            stroke_color: None,
            stroke_size: 0.0,
            opacity: 1.0,
            style: kind,
        }
    }

    /// 落在纸上的墨有多少（不透明像素计数），粗体那一张该比常规的多
    fn ink(bitmap: &TextBitmap) -> usize {
        bitmap.image.pixels().filter(|pixel| pixel[3] > 200).count()
    }

    /// 粗体这一档必须真的换了东西画：随包那份有 Bold 时走真粗体面，
    /// 没有才退回四亚像素叠画。两条路都该比常规更墨——这条红了就是 `bold` 那一档
    /// 在渲染链上被丢掉了（比如 `face()` 忘了传，或 `synthesized` 恒假）。
    #[test]
    fn the_bold_style_draws_heavier_than_regular() {
        let font = FontSet::detect("").expect("本机该有一份能渲染中文的字体");
        let regular = ink(&render_text(
            &font,
            "戳一戳测试",
            &style(TextStyleKind::Plain),
        ));
        let bold = ink(&render_text(
            &font,
            "戳一戳测试",
            &style(TextStyleKind::Bold),
        ));
        assert!(regular > 0, "常规那一张该有墨");
        assert!(
            bold > regular,
            "粗体 {bold} 像素没比常规 {regular} 多（字面里有真粗体：{}）",
            font.has_bold()
        );
    }

    /// 描边与斜体这两档不能被排版改动弄丢：描边那张比裸字大，斜体那张的墨与正体同量级
    #[test]
    fn stroke_and_slant_still_change_the_picture() {
        let font = FontSet::detect("").expect("本机该有一份能渲染中文的字体");
        let plain = ink(&render_text(&font, "戳一戳", &style(TextStyleKind::Plain)));
        let mut outlined = style(TextStyleKind::Plain);
        outlined.stroke_color = Some([255, 255, 255, 255]);
        outlined.stroke_size = 3.0;
        let stroke = ink(&render_text(&font, "戳一戳", &outlined));
        assert!(stroke > plain, "描边那一张该比裸字大：{stroke} vs {plain}");
        let slanted = ink(&render_text(&font, "戳一戳", &style(TextStyleKind::Italic)));
        assert!(slanted > 0, "斜体那一张不该是空的");
    }
}

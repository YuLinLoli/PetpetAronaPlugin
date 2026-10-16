//! 渲染管线：模板 + 请求数据 → PNG / GIF 字节。
//!
//! 覆盖 petpet-core 的常用能力：多层图像/文本、coords、fit、crop、圆角、旋转、透明度、
//! 常用滤镜、GIF 多帧。透视变形(deform)与高级滤镜已降级处理，见模块内注释。

use std::collections::HashMap;
use std::sync::Arc;

use image::{
    Delay, Frame, Rgba, RgbaImage,
    codecs::gif::{GifEncoder, Repeat},
};

use crate::assets::{AssetStore, Frames};
use crate::imageops;
use crate::length::{Axis, Ctx, Node};
use crate::template::{
    self, Coords, Crop, Element, Filter, ImageElement, Kind, Template, TextElement,
};
use crate::text::{self, FontSet, TextStyle};

/// 一次渲染的请求数据（键 → 图像地址 / 文本）
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub images: HashMap<String, String>,
    pub texts: HashMap<String, String>,
}

/// 渲染结果
pub struct Output {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
    pub frames: usize,
}

/// 预载好的图像元素源
struct ImageSource {
    /// 按帧取用的变体（src 数组或单一请求图像）
    variants: Vec<Arc<Frames>>,
    matched_key: Option<String>,
    /// 与 [`Template::elements`] 对应的下标
    index: usize,
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

pub async fn render(
    template: &Template,
    request: &Request,
    assets: &AssetStore,
    font: Option<&FontSet>,
    max_frames: u32,
    max_side: u32,
) -> Result<Output, String> {
    let mut sources: HashMap<usize, ImageSource> = HashMap::new();
    let mut backgrounds: HashMap<usize, Arc<Frames>> = HashMap::new();

    for (index, element) in template.elements.iter().enumerate() {
        match element {
            Element::Image(image) => {
                if let Some(source) =
                    load_image_source(template, image, index, request, assets).await?
                {
                    sources.insert(index, source);
                }
            }
            Element::Background(background) => {
                let src = background
                    .srcs
                    .first()
                    .cloned()
                    .unwrap_or_else(|| template.dir.to_string_lossy().to_string());
                match assets.load(&template.dir, &src).await {
                    Ok(Some(frames)) => {
                        backgrounds.insert(index, frames);
                    }
                    Ok(None) => {}
                    Err(err) => eprintln!("petpet: 背景 {src} 加载失败({err})，已跳过"),
                }
            }
            Element::Text(_) => {}
        }
    }

    // 元素尺寸变量（`to_width` / `avatar0_height`）要先登记：旧版模板常用
    // `background.size = ["avatar0Width*2", "avatar0Height"]` 反推画布尺寸。
    let mut ctx = Ctx::new(0.0, 0.0);
    register_element_sizes(&mut ctx, template, &sources, &backgrounds);

    // 画布尺寸：先按常量/元素变量求值，缺失时退回背景图尺寸
    let mut canvas_w = eval_count(&template.canvas.width, &ctx, Axis::X);
    let mut canvas_h = eval_count(&template.canvas.height, &ctx, Axis::Y);
    if (canvas_w <= 0.0 || canvas_h <= 0.0)
        && let Some(frames) = backgrounds.values().next()
    {
        let size = frames.sample(0).dimensions();
        if canvas_w <= 0.0 {
            canvas_w = size.0 as f32;
        }
        if canvas_h <= 0.0 {
            canvas_h = size.1 as f32;
        }
    }
    if canvas_w <= 0.0 {
        canvas_w = 300.0;
    }
    if canvas_h <= 0.0 {
        canvas_h = 300.0;
    }
    ctx.set_canvas(canvas_w, canvas_h);
    // 画布定下来后再算一次，让依赖 `vw` / `vh` 的坐标也用上真实画布尺寸
    register_element_sizes(&mut ctx, template, &sources, &backgrounds);

    // 帧数：画布声明与各资源长度取最大
    let declared = match &template.canvas.length {
        Some(node) => node
            .fixed_px()
            .or_else(|| Some(ctx.eval(node, Axis::X)))
            .unwrap_or(1.0)
            .round()
            .max(1.0) as usize,
        None => 1,
    };
    let mut natural = 1usize;
    for source in sources.values() {
        for variant in &source.variants {
            natural = natural.max(variant.len());
        }
    }
    for frames in backgrounds.values() {
        natural = natural.max(frames.len());
    }
    for element in &template.elements {
        if let Element::Text(text) = element {
            natural = natural.max(text.text.len());
            natural = natural.max(text.coords.len());
            natural = natural.max(text.color.len());
        }
    }
    let mut frames = declared.max(natural).max(1);
    if template.kind == Kind::Image {
        frames = 1;
    }
    frames = frames.min(max_frames.max(1) as usize);

    // 输出缩放比先算好，逐帧「渲染 → 缩放 → 编码 → 释放」，不再把所有帧同时留在内存里
    // （默认 120 帧 × 1200² 的整组缓存在最坏情况下会到 GB 级）。
    let scale = if max_side > 0 {
        let longest = canvas_w.max(canvas_h);
        if longest > max_side as f32 {
            max_side as f32 / longest
        } else {
            1.0
        }
    } else {
        1.0
    };
    let delay_at = |index: usize| -> u32 {
        if template.delays.is_empty() {
            65
        } else {
            template.delays[index % template.delays.len()].max(1)
        }
    };
    let renderer = FrameRenderer {
        template,
        request,
        assets,
        font,
        sources: &sources,
        backgrounds: &backgrounds,
        ctx: &ctx,
        canvas_w,
        canvas_h,
        frames,
    };

    if template.kind == Kind::Gif || frames > 1 {
        let mut buffer = Vec::new();
        {
            let mut encoder = GifEncoder::new_with_speed(&mut buffer, 10);
            // 与原版一致：GIF 无限循环。image 的编码器默认不写 NETSCAPE2.0 循环块，
            // 不显式设这一句，实现端只会播一遍。
            encoder
                .set_repeat(Repeat::Infinite)
                .map_err(|err| format!("设置 GIF 循环失败: {err}"))?;
            for out_index in 0..frames {
                let source_index = if template.canvas.reverse {
                    frames - 1 - out_index
                } else {
                    out_index
                };
                let canvas = downscale(renderer.render(source_index).await, scale);
                let gif_frame = Frame::from_parts(
                    canvas,
                    0,
                    0,
                    Delay::from_numer_denom_ms(delay_at(out_index), 1),
                );
                encoder
                    .encode_frame(gif_frame)
                    .map_err(|err| format!("编码 GIF 失败: {err}"))?;
            }
        }
        Ok(Output {
            bytes: buffer,
            extension: "gif",
            frames,
        })
    } else {
        let canvas = downscale(renderer.render(0).await, scale);
        let mut buffer = Vec::new();
        canvas
            .write_to(
                &mut std::io::Cursor::new(&mut buffer),
                image::ImageFormat::Png,
            )
            .map_err(|err| format!("编码 PNG 失败: {err}"))?;
        Ok(Output {
            bytes: buffer,
            extension: "png",
            frames: 1,
        })
    }
}
fn eval_count(node: &Option<Node>, ctx: &Ctx, axis: Axis) -> f32 {
    node.as_ref()
        .map(|node| ctx.eval(node, axis))
        .unwrap_or(0.0)
}

/// 计算每个元素的目标尺寸并登记成 `{名字}_width` / `{名字}_height` 变量。
/// 旧版模板的 `background.size` 会引用 `avatar0Width` 这类名字，必须在解析画布前先算好。
fn register_element_sizes(
    ctx: &mut Ctx,
    template: &Template,
    sources: &HashMap<usize, ImageSource>,
    backgrounds: &HashMap<usize, Arc<Frames>>,
) {
    for (index, element) in template.elements.iter().enumerate() {
        let name = element_name(element, index);
        let size = match element {
            Element::Image(image) => sources
                .get(&index)
                .and_then(|source| source.variants.first())
                .map(|frames| {
                    let original = frames.sample(0).dimensions();
                    let mut local = ctx.clone();
                    local.set_element(original.0 as f32, original.1 as f32);
                    let coords = template::pick(&image.coords, 0)
                        .cloned()
                        .unwrap_or_else(template::default_coords);
                    element_size(&coords, &local, original)
                })
                .unwrap_or((0.0, 0.0)),
            Element::Background(_) => backgrounds
                .values()
                .next()
                .map(|frames| {
                    let size = frames.sample(0).dimensions();
                    (size.0 as f32, size.1 as f32)
                })
                .unwrap_or((0.0, 0.0)),
            Element::Text(_) => (0.0, 0.0),
        };
        ctx.put(format!("{name}_width"), size.0);
        ctx.put(format!("{name}_height"), size.1);
        if let Element::Image(image) = element {
            for key in &image.keys {
                ctx.put(format!("{key}_width"), size.0);
                ctx.put(format!("{key}_height"), size.1);
            }
        }
    }
}
fn element_name(element: &Element, index: usize) -> String {
    match element {
        Element::Image(image) => image
            .id
            .clone()
            .unwrap_or_else(|| format!("element_{index}")),
        Element::Text(text) => text
            .id
            .clone()
            .unwrap_or_else(|| format!("element_{index}")),
        Element::Background(_) => format!("element_{index}"),
    }
}

async fn load_image_source(
    template: &Template,
    image: &ImageElement,
    index: usize,
    request: &Request,
    assets: &AssetStore,
) -> Result<Option<ImageSource>, String> {
    // 请求数据优先：任一 key 命中就用它
    for key in &image.keys {
        if let Some(src) = request.images.get(key)
            && let Ok(Some(frames)) = assets.load(&template.dir, src).await
        {
            return Ok(Some(ImageSource {
                variants: vec![frames],
                matched_key: Some(key.clone()),
                index,
            }));
        }
    }
    if image.srcs.is_empty() {
        return Ok(None);
    }
    let mut variants = Vec::with_capacity(image.srcs.len());
    for src in &image.srcs {
        match assets.load(&template.dir, src).await {
            Ok(Some(frames)) => variants.push(frames),
            Ok(None) => {}
            Err(err) => eprintln!("petpet: 元素素材 {src} 加载失败({err})，已跳过"),
        }
    }
    if variants.is_empty() {
        return Ok(None);
    }
    Ok(Some(ImageSource {
        variants,
        matched_key: None,
        index,
    }))
}

/// 元素的目标尺寸（XYWH 直接求值；P4A 取四角包围盒）
fn element_size(coords: &Coords, ctx: &Ctx, src: (u32, u32)) -> (f32, f32) {
    match coords {
        Coords::Empty => (0.0, 0.0),
        Coords::Xywh { w, h, .. } => {
            let mut wv = ctx.eval(w, Axis::X);
            let mut hv = ctx.eval(h, Axis::Y);
            let (sw, sh) = (src.0 as f32, src.1 as f32);
            if wv <= 0.0 && hv <= 0.0 {
                wv = sw;
                hv = sh;
            } else if wv <= 0.0 {
                wv = if sh > 0.0 { hv * sw / sh } else { hv };
            } else if hv <= 0.0 {
                hv = if sw > 0.0 { wv * sh / sw } else { wv };
            }
            (wv, hv)
        }
        Coords::P4a { points, .. } => {
            let mut min_x = f32::INFINITY;
            let mut min_y = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;
            let mut max_y = f32::NEG_INFINITY;
            for point in points {
                let x = ctx.eval(&point[0], Axis::X);
                let y = ctx.eval(&point[1], Axis::Y);
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
            ((max_x - min_x).max(0.0), (max_y - min_y).max(0.0))
        }
    }
}

/// 求一个 XYWH 元素的目标矩形；宽高缺省时按（裁切后的）源图比例补全
fn resolve_rect(coords: &Coords, ctx: &Ctx, src: (u32, u32)) -> Rect {
    match coords {
        Coords::Xywh { x, y, .. } => {
            let (w, h) = element_size(coords, ctx, src);
            Rect {
                x: ctx.eval(x, Axis::X),
                y: ctx.eval(y, Axis::Y),
                w,
                h,
            }
        }
        _ => Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        },
    }
}

/// 预载 `Mirage` 滤镜需要的「内层图」，顺序与 `image.filters` 中出现的顺序一致
async fn load_mirages(
    template: &Template,
    image: &ImageElement,
    request: &Request,
    assets: &AssetStore,
    frame: usize,
) -> Vec<Option<RgbaImage>> {
    let mut out = Vec::new();
    for filter in &image.filters {
        let Filter::Mirage(spec) = filter else {
            continue;
        };
        let mut loaded = None;
        for key in &spec.keys {
            if let Some(src) = request.images.get(key)
                && let Ok(Some(frames)) = assets.load(&template.dir, src).await
            {
                loaded = Some(frames.sample(frame).clone());
                break;
            }
        }
        if loaded.is_none() {
            for src in &spec.srcs {
                if let Ok(Some(frames)) = assets.load(&template.dir, src).await {
                    loaded = Some(frames.sample(frame).clone());
                    break;
                }
            }
        }
        out.push(loaded);
    }
    out
}

/// 按 `crop` 配置裁切（对齐 petpet 的 `ImageXYWHCropper` / `ImagePolygonCropper`）
fn apply_crop(pixmap: &RgbaImage, crop: &Crop, ctx: &Ctx) -> RgbaImage {
    match crop {
        Crop::Xywh { x1, y1, x2, y2 } => {
            let x1 = ctx.eval(x1, Axis::X).round() as i32;
            let y1 = ctx.eval(y1, Axis::Y).round() as i32;
            let x2 = ctx.eval(x2, Axis::X).round() as i32;
            let y2 = ctx.eval(y2, Axis::Y).round() as i32;
            imageops::crop_rect(pixmap, x1, y1, x2, y2)
        }
        Crop::Polygon(points) => {
            let evaluated: Vec<(f32, f32)> = points
                .iter()
                .map(|point| (ctx.eval(&point[0], Axis::X), ctx.eval(&point[1], Axis::Y)))
                .collect();
            imageops::crop_polygon(pixmap, &evaluated)
        }
    }
}

/// 元素在帧区间 `[start, end]` 内才绘制（对齐 petpet 的 `start`/`end`，`end < 0` 表示到末尾）
fn frame_in_range(start: i64, end: i64, total: usize, frame: usize) -> bool {
    let frame = frame as i64;
    if frame < start {
        return false;
    }
    let end = if end < 0 { total as i64 + end + 1 } else { end };
    frame <= end
}

fn draw_image(
    canvas: &mut RgbaImage,
    image: &ImageElement,
    source: &ImageSource,
    frame: usize,
    total_frames: usize,
    base_ctx: &Ctx,
    mirages: &[Option<RgbaImage>],
) -> Result<(), String> {
    if !frame_in_range(image.start, image.end, total_frames, frame) {
        return Ok(());
    }
    // `reverse`：该元素的帧序列倒序播放
    let frame = if image.reverse && total_frames > 0 {
        total_frames - 1 - frame.min(total_frames - 1)
    } else {
        frame
    };
    let variant = &source.variants[frame.min(source.variants.len() - 1)];
    let mut pixmap = variant.sample(frame).clone();
    let original = pixmap.dimensions();

    // petpet：元素内坐标（含 `%`/`cw`/`ch`）以元素自身尺寸为基准
    let mut ctx = base_ctx.clone();
    ctx.set_element(original.0 as f32, original.1 as f32);

    let coords = template::pick(&image.coords, frame)
        .cloned()
        .unwrap_or_else(template::default_coords);
    if matches!(coords, Coords::Empty) {
        return Ok(());
    }

    let mut angle = template::pick(&image.angle, frame).copied().unwrap_or(0.0);
    if let Some(rotate) = &image.rotate {
        angle = rotate.angle(total_frames, frame);
    }

    if let Some(crop) = &image.crop {
        pixmap = apply_crop(&pixmap, crop, &ctx);
    }

    let assets = imageops::FilterAssets {
        mirages: mirages.to_vec(),
        frame,
    };
    imageops::apply_filters(&mut pixmap, &image.filters, &assets);

    let opacity = template::pick(&image.opacity, frame)
        .copied()
        .unwrap_or(1.0);

    match &coords {
        Coords::Xywh { .. } => {
            let rect = resolve_rect(&coords, &ctx, pixmap.dimensions());
            if rect.w <= 0.0 || rect.h <= 0.0 {
                return Ok(());
            }
            let mut fitted = imageops::fit_into(
                &pixmap,
                rect.w.round().max(1.0) as u32,
                rect.h.round().max(1.0) as u32,
                image.fit,
                &image.position,
            );
            if let Some(radius) = template::pick(&image.radius, frame) {
                let values = imageops::radius_values(radius, fitted.width(), fitted.height(), &ctx);
                imageops::round_corners(&mut fitted, values);
            }
            let mut paste_x = rect.x;
            let mut paste_y = rect.y;
            if angle.abs() >= 0.01 {
                let (ox, oy) = image.origin.eval(rect.w, rect.h);
                let (rotated, min_x, min_y) = imageops::rotate_about_px(&fitted, angle, ox, oy);
                fitted = rotated;
                paste_x = rect.x + min_x;
                paste_y = rect.y + min_y;
            }
            imageops::paste(
                canvas,
                &fitted,
                paste_x.round() as i32,
                paste_y.round() as i32,
                opacity,
            );
        }
        Coords::P4a { points, anchor } => {
            let mut evaluated = [[0.0f32; 2]; 4];
            for (index, point) in points.iter().enumerate() {
                evaluated[index] = [ctx.eval(&point[0], Axis::X), ctx.eval(&point[1], Axis::Y)];
            }
            let anchor_point = (ctx.eval(&anchor[0], Axis::X), ctx.eval(&anchor[1], Axis::Y));
            let (points_abs, anchor_abs) = imageops::absolute_coords(evaluated, anchor_point);
            let element_size = (pixmap.width() as f32, pixmap.height() as f32);
            if let Some(radius) = template::pick(&image.radius, frame) {
                let values = imageops::radius_values(radius, pixmap.width(), pixmap.height(), &ctx);
                imageops::round_corners(&mut pixmap, values);
            }
            let mut deformed = imageops::deform(&pixmap, points_abs);
            let mut paste_x = anchor_abs.0;
            let mut paste_y = anchor_abs.1;
            if angle.abs() >= 0.01 {
                let (ox, oy) = image.origin.eval(element_size.0, element_size.1);
                let (rotated, min_x, min_y) = imageops::rotate_about_px(&deformed, angle, ox, oy);
                deformed = rotated;
                paste_x += min_x;
                paste_y += min_y;
            }
            imageops::paste(
                canvas,
                &deformed,
                paste_x.round() as i32,
                paste_y.round() as i32,
                opacity,
            );
        }
        Coords::Empty => {}
    }
    Ok(())
}

fn draw_text(
    canvas: &mut RgbaImage,
    element: &TextElement,
    frame: usize,
    total_frames: usize,
    ctx: &Ctx,
    request: &Request,
    font: &FontSet,
) -> Result<(), String> {
    if !frame_in_range(element.start, element.end, total_frames, frame) {
        return Ok(());
    }
    let raw = template::pick(&element.text, frame)
        .cloned()
        .unwrap_or_default();
    let content = substitute(&raw, request);
    let coords = template::pick(&element.coords, frame)
        .cloned()
        .unwrap_or(template::TextCoords {
            x: Node::px(0.0),
            y: Node::px(0.0),
            max_w: None,
        });
    let x = ctx.eval(&coords.x, Axis::X);
    let y = ctx.eval(&coords.y, Axis::Y);
    let max_w = coords.max_w.as_ref().map(|node| ctx.eval(node, Axis::X));
    let color = template::pick(&element.color, frame)
        .copied()
        .unwrap_or([255, 255, 255, 255]);
    let size = template::pick(&element.size, frame)
        .copied()
        .unwrap_or(16.0);
    let max_size = template::pick(&element.max_size, frame)
        .copied()
        .unwrap_or(96.0);
    let angle = template::pick(&element.angle, frame)
        .copied()
        .unwrap_or(0.0);
    let opacity = template::pick(&element.opacity, frame)
        .copied()
        .unwrap_or(1.0);

    let style = TextStyle {
        size,
        max_size,
        color,
        align: element.align,
        baseline: element.baseline,
        wrap: element.wrap,
        max_width: max_w,
        stroke_color: element.stroke_color,
        stroke_size: element.stroke_size,
        opacity,
        style: element.style,
    };
    let text_bitmap = text::render_text(font, &content, &style);
    let mut bitmap = text_bitmap.image;
    let mut paste_x = x - text_bitmap.anchor_x;
    let mut paste_y = y - text_bitmap.anchor_y;
    if angle.abs() >= 0.01 {
        // petpet：旋转中心 = 文本锚点 + origin 偏移
        let (ox, oy) = element
            .origin
            .eval(bitmap.width() as f32, bitmap.height() as f32);
        let center_x = paste_x + ox;
        let center_y = paste_y + oy;
        let (rotated, min_x, min_y) = imageops::rotate_about_px(&bitmap, angle, ox, oy);
        bitmap = rotated;
        paste_x = center_x - (ox - min_x);
        paste_y = center_y - (oy - min_y);
    }
    imageops::paste(
        canvas,
        &bitmap,
        paste_x.round() as i32,
        paste_y.round() as i32,
        1.0,
    );
    Ok(())
}

/// 文本变量替换，语义对齐 petpet 的 `TextStringTemplate`：
/// - `${name}` / `${name:default}` / `${name:-default}` / `${name:=default}` / `${name:+default}`
/// - `$name` 短格式（字母、数字、下划线）
/// - `\x` 转义为字面量 `x`
/// - 默认值本身也是模板，会递归展开
pub fn substitute(raw: &str, request: &Request) -> String {
    substitute_inner(raw, request, 0)
}

fn substitute_inner(raw: &str, request: &Request, depth: usize) -> String {
    if depth > 8 {
        return raw.to_string();
    }
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        if current == '\\' {
            match chars.get(index + 1) {
                Some(next) => {
                    out.push(*next);
                    index += 2;
                }
                None => {
                    out.push(current);
                    index += 1;
                }
            }
            continue;
        }
        if current != '$' {
            out.push(current);
            index += 1;
            continue;
        }
        match chars.get(index + 1) {
            Some('{') => {
                // 允许默认值里再嵌套 `${...}`
                let mut nesting = 0usize;
                let mut cursor = index + 2;
                let mut closing = None;
                while cursor < chars.len() {
                    match chars[cursor] {
                        '{' => nesting += 1,
                        '}' => {
                            if nesting == 0 {
                                closing = Some(cursor);
                                break;
                            }
                            nesting -= 1;
                        }
                        _ => {}
                    }
                    cursor += 1;
                }
                let Some(closing) = closing else {
                    out.push(current);
                    index += 1;
                    continue;
                };
                let content: String = chars[index + 2..closing].iter().collect();
                let (name, default) = split_variable(&content);
                let value = request
                    .texts
                    .get(name)
                    .filter(|value| !value.is_empty())
                    .cloned()
                    .or_else(|| {
                        default.map(|default| substitute_inner(default, request, depth + 1))
                    })
                    .unwrap_or_default();
                out.push_str(&value);
                index = closing + 1;
            }
            Some(next) if next.is_alphanumeric() || *next == '_' => {
                let mut cursor = index + 1;
                while cursor < chars.len()
                    && (chars[cursor].is_alphanumeric() || chars[cursor] == '_')
                {
                    cursor += 1;
                }
                let name: String = chars[index + 1..cursor].iter().collect();
                if let Some(value) = request.texts.get(&name) {
                    out.push_str(value);
                }
                index = cursor;
            }
            _ => {
                out.push(current);
                index += 1;
            }
        }
    }
    out
}

/// 拆 `${name:default}`：`:-` / `:=` / `:+` 三种前缀都视作默认值标记（与原版一致）
fn split_variable(content: &str) -> (&str, Option<&str>) {
    match content.split_once(':') {
        Some((name, rest)) => {
            let default = match rest.chars().next() {
                Some('-') | Some('=') | Some('+') => &rest[1..],
                _ => rest,
            };
            (name, Some(default))
        }
        None => (content, None),
    }
}
/// 等比缩小到最大边长以内；`scale == 1` 时原样返回，避免多余的一次拷贝
fn downscale(image: RgbaImage, scale: f32) -> RgbaImage {
    if (scale - 1.0).abs() < f32::EPSILON {
        return image;
    }
    let (w, h) = image.dimensions();
    imageops::resize(
        &image,
        (w as f32 * scale).round().max(1.0) as u32,
        (h as f32 * scale).round().max(1.0) as u32,
    )
}

/// 单帧渲染器：只借用渲染一帧所需的上下文，让上层可以逐帧编码、及时释放
struct FrameRenderer<'a> {
    template: &'a Template,
    request: &'a Request,
    assets: &'a AssetStore,
    font: Option<&'a FontSet>,
    sources: &'a HashMap<usize, ImageSource>,
    backgrounds: &'a HashMap<usize, Arc<Frames>>,
    ctx: &'a Ctx,
    canvas_w: f32,
    canvas_h: f32,
    frames: usize,
}

impl FrameRenderer<'_> {
    async fn render(&self, frame: usize) -> RgbaImage {
        let mut canvas = RgbaImage::from_pixel(
            self.canvas_w.round().max(1.0) as u32,
            self.canvas_h.round().max(1.0) as u32,
            Rgba(self.template.canvas.color),
        );
        for (index, element) in self.template.elements.iter().enumerate() {
            match element {
                Element::Background(_) => {
                    if let Some(frames) = self.backgrounds.get(&index) {
                        let source = frames.sample(frame);
                        let scaled = imageops::resize(source, canvas.width(), canvas.height());
                        imageops::paste(&mut canvas, &scaled, 0, 0, 1.0);
                    }
                }
                Element::Image(image) => {
                    let Some(source) = self.sources.get(&index) else {
                        continue;
                    };
                    let mirages =
                        load_mirages(self.template, image, self.request, self.assets, frame).await;
                    if let Err(err) = draw_image(
                        &mut canvas,
                        image,
                        source,
                        frame,
                        self.frames,
                        self.ctx,
                        &mirages,
                    ) {
                        eprintln!("petpet: 绘制图像元素失败: {err}");
                    }
                }
                Element::Text(text_element) => {
                    let Some(font) = self.font else {
                        continue;
                    };
                    if let Err(err) = draw_text(
                        &mut canvas,
                        text_element,
                        frame,
                        self.frames,
                        self.ctx,
                        self.request,
                        font,
                    ) {
                        eprintln!("petpet: 绘制文本元素失败: {err}");
                    }
                }
            }
        }
        canvas
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("petpet_render_e2e_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_png(path: &Path, w: u32, h: u32, color: [u8; 4]) {
        RgbaImage::from_pixel(w, h, Rgba(color)).save(path).unwrap();
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(future)
    }

    /// 统计与给定底色明显不同的像素数，用来确认真的画上了东西
    fn changed_pixels(image: &RgbaImage, base: [u8; 4]) -> usize {
        image
            .pixels()
            .filter(|pixel| {
                let [r, g, b, _] = pixel.0;
                (r as i32 - base[0] as i32).abs()
                    + (g as i32 - base[1] as i32).abs()
                    + (b as i32 - base[2] as i32).abs()
                    > 30
            })
            .count()
    }

    #[test]
    fn renders_gif_template_end_to_end() {
        let dir = scratch("gif");
        write_png(&dir.join("bg.png"), 200, 150, [12, 24, 36, 255]);
        write_png(&dir.join("avatar.png"), 80, 80, [200, 120, 40, 255]);
        std::fs::write(
            dir.join("template.json"),
            r##"{
              "type": "gif",
              "metadata": {"alias": ["demo"]},
              "canvas": {"width": 200, "height": 150, "length": 3},
              "delay": [50, 60, 70],
              "elements": [
                {"type": "background", "src": "bg.png"},
                {"type": "image", "key": "to", "coords": [10, 10, 60, 60], "fit": "cover"},
                {"type": "text", "text": "Hi ${to}", "coords": [20, 120], "color": "#ffffff", "size": 24}
              ]
            }"##,
        )
        .unwrap();

        let template = crate::template::load_dir("demo", &dir).unwrap();
        assert_eq!(template.kind, Kind::Gif);
        assert_eq!(template.delays, vec![50, 60, 70]);
        assert!(template.image_keys.contains("to"));

        let mut request = Request::default();
        request.images.insert(
            "to".to_string(),
            dir.join("avatar.png").to_string_lossy().to_string(),
        );
        request.texts.insert("to".to_string(), "Arona".to_string());

        let assets = AssetStore::new(8);
        let font = crate::text::FontSet::detect("");
        let output = block_on(render(
            &template,
            &request,
            &assets,
            font.as_ref(),
            60,
            1024,
        ))
        .unwrap();

        assert_eq!(output.extension, "gif");
        assert_eq!(output.frames, 3);
        assert!(output.bytes.starts_with(b"GIF8"), "输出应为 GIF 字节流");
        assert!(
            output.bytes.windows(11).any(|w| w == b"NETSCAPE2.0"),
            "GIF 应写入 NETSCAPE2.0 循环块（无限循环）"
        );

        let decoded = image::load_from_memory(&output.bytes).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (200, 150));
        assert!(
            changed_pixels(&decoded, [12, 24, 36, 255]) > 500,
            "应绘制出头像 / 文本像素"
        );
    }

    /// 内联输入图（data URI）能直接参与渲染，不走本地文件
    #[test]
    fn renders_inline_data_uri_image() {
        let dir = scratch("inline_data_uri");
        write_png(&dir.join("bg.png"), 120, 120, [0, 0, 0, 255]);
        std::fs::write(
            dir.join("template.json"),
            r##"{
              "type": "png",
              "canvas": {"width": 120, "height": 120},
              "elements": [
                {"type": "background", "src": "bg.png"},
                {"type": "image", "key": "to", "coords": [10, 10, 100, 100], "fit": "cover"}
              ]
            }"##,
        )
        .unwrap();
        let template = crate::template::load_dir("inline", &dir).unwrap();

        // 造一张纯色 PNG，再编成 data URI 当作输入图
        let avatar = RgbaImage::from_pixel(64, 64, Rgba([250, 30, 30, 255]));
        let mut png = Vec::new();
        {
            use image::ImageEncoder as _;
            image::codecs::png::PngEncoder::new(&mut png)
                .write_image(avatar.as_raw(), 64, 64, image::ExtendedColorType::Rgba8)
                .unwrap();
        }
        let uri = crate::assets::data_uri("image/png", &png);

        let mut request = Request::default();
        request.images.insert("to".to_string(), uri);

        let assets = AssetStore::new(8);
        let output = block_on(render(&template, &request, &assets, None, 60, 1024)).unwrap();
        assert_eq!(output.extension, "png");
        let decoded = image::load_from_memory(&output.bytes).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (120, 120));
        assert!(
            changed_pixels(&decoded, [0, 0, 0, 255]) > 1000,
            "内联头像应被画上"
        );
    }

    /// 旧版「右对称」：把素材右半边镜像铺满整张图，画布尺寸由 `avatar0Width*2` 反推。
    /// 之前 `avatar0Width` 被错误替换成 `avatar0_100cw`，画布解析成 0 后回退，整张图跑偏。
    #[test]
    fn renders_legacy_symmetry_template() {
        let dir = scratch("legacy_symmetry");
        std::fs::write(
            dir.join("data.json"),
            r##"{
              "type": "IMG",
              "avatar": [{
                "type": "TO",
                "pos": ["width/2", 0, "width/2", "height"],
                "crop": [50, 0, 100, 100],
                "cropType": "PERCENT",
                "style": ["MIRROR"],
                "round": false,
                "avatarOnTop": true
              }, {
                "type": "TO",
                "pos": [0, 0, "width/2", "height"],
                "crop": [50, 0, 100, 100],
                "cropType": "PERCENT",
                "style": ["MIRROR"],
                "round": false,
                "avatarOnTop": true
              }],
              "text": [],
              "background": { "size": ["avatar0Width*2", "avatar0Height"] },
              "alias": ["对称", "右对称", "左右对称"],
              "inRandomList": false
            }"##,
        )
        .unwrap();

        // 素材 200x100：右半边用红色通道编码横坐标，便于验证镜像方向
        let mut input = RgbaImage::new(200, 100);
        for (x, _y, pixel) in input.enumerate_pixels_mut() {
            *pixel = if x < 100 {
                Rgba([10, 0, 0, 255])
            } else {
                Rgba([x as u8, 0, 0, 255])
            };
        }
        let input_path = dir.join("input.png");
        input.save(&input_path).unwrap();

        let template = crate::template::load_dir("sym", &dir).unwrap();
        let mut request = Request::default();
        request
            .images
            .insert("to".to_string(), input_path.to_string_lossy().to_string());

        let assets = AssetStore::new(8);
        let font = crate::text::FontSet::detect("");
        let output = block_on(render(
            &template,
            &request,
            &assets,
            font.as_ref(),
            60,
            4096,
        ))
        .unwrap();
        let decoded = image::load_from_memory(&output.bytes).unwrap().to_rgba8();

        // 画布应等于素材尺寸（`avatar0Width*2` x `avatar0Height`），而不是回退到 300
        assert_eq!(
            decoded.dimensions(),
            (200, 100),
            "画布应由 avatar0Width*2 反推"
        );

        for x in 0..100u32 {
            let left = decoded.get_pixel(x, 50).0[0];
            let right = decoded.get_pixel(x + 100, 50).0[0];
            assert_eq!(left, right, "左右两半应完全一致（x={x}）");
            assert_eq!(left, (199 - x) as u8, "两半都应是素材右半边的镜像（x={x}）");
        }
    }
    /// 旧版「上下对称」：把素材上半边镜像铺满整张图，画布尺寸由 `avatar0Height*2` 反推。
    #[test]
    fn renders_legacy_vertical_symmetry_template() {
        let dir = scratch("legacy_v_symmetry");
        std::fs::write(
            dir.join("data.json"),
            r##"{
              "type": "IMG",
              "avatar": [{
                "type": "TO",
                "pos": [0, 0, "width", "height/2"],
                "crop": [0, 0, 100, 50],
                "cropType": "PERCENT",
                "round": false,
                "avatarOnTop": true
              }, {
                "type": "TO",
                "pos": [0, "height/2", "width", "height/2"],
                "crop": [0, 0, 100, 50],
                "cropType": "PERCENT",
                "style": ["FLIP"],
                "round": false,
                "avatarOnTop": true
              }],
              "text": [],
              "background": { "size": ["avatar0Width", "avatar0Height*2"] },
              "alias": ["对称", "上对称", "上下对称"],
              "inRandomList": false
            }"##,
        )
        .unwrap();

        // 素材 100x200：上半边用红色通道编码纵坐标，便于验证上下镜像
        let mut input = RgbaImage::new(100, 200);
        for (_x, y, pixel) in input.enumerate_pixels_mut() {
            *pixel = Rgba([y as u8, 0, 0, 255]);
        }
        let input_path = dir.join("input.png");
        input.save(&input_path).unwrap();

        let template = crate::template::load_dir("sym", &dir).unwrap();
        let mut request = Request::default();
        request
            .images
            .insert("to".to_string(), input_path.to_string_lossy().to_string());

        let assets = AssetStore::new(8);
        let font = crate::text::FontSet::detect("");
        let output = block_on(render(
            &template,
            &request,
            &assets,
            font.as_ref(),
            60,
            4096,
        ))
        .unwrap();
        let decoded = image::load_from_memory(&output.bytes).unwrap().to_rgba8();

        assert_eq!(
            decoded.dimensions(),
            (100, 200),
            "画布应由 avatar0Height*2 反推"
        );
        for y in 0..100u32 {
            let top = decoded.get_pixel(50, y).0[0];
            let bottom = decoded.get_pixel(50, y + 100).0[0];
            assert_eq!(top, y as u8, "上半边应保留素材上边（y={y}）");
            assert_eq!(bottom, (99 - y) as u8, "下半边应是上半边的镜像（y={y}）");
        }
    }
    /// 旧版「中心对称」：4 个头像引用 `avatar0..avatar3`，画布由 `avatar0Width*2` 反推。
    #[test]
    fn renders_legacy_center_symmetry_template() {
        let dir = scratch("legacy_c_symmetry");
        std::fs::write(
            dir.join("data.json"),
            r##"{
              "type": "IMG",
              "avatar": [{
                "type": "TO", "pos": [0, 0, "width/2", "height/2"],
                "crop": [0, 0, 50, 50], "cropType": "PERCENT", "round": false, "avatarOnTop": true
              }, {
                "type": "TO", "pos": ["width/2", 0, "width/2", "height/2"],
                "crop": [0, 0, 50, 50], "cropType": "PERCENT", "style": ["MIRROR"],
                "round": false, "avatarOnTop": true
              }, {
                "type": "TO", "pos": [0, "height/2", "width/2", "height/2"],
                "crop": [0, 0, 50, 50], "cropType": "PERCENT", "style": ["FLIP"],
                "round": false, "avatarOnTop": true
              }, {
                "type": "TO", "pos": ["width/2", "height/2", "width/2", "height/2"],
                "crop": [0, 0, 50, 50], "cropType": "PERCENT", "style": ["MIRROR", "FLIP"],
                "round": false, "avatarOnTop": true
              }],
              "text": [],
              "background": { "size": ["avatar0Width*2", "avatar0Height*2"] },
              "alias": ["中心对称", "左上对称"],
              "inRandomList": false
            }"##,
        )
        .unwrap();

        // 素材 200x100：R 编码 x、G 编码 y
        let mut input = RgbaImage::new(200, 100);
        for (x, y, pixel) in input.enumerate_pixels_mut() {
            *pixel = Rgba([x as u8, y as u8, 0, 255]);
        }
        let input_path = dir.join("input.png");
        input.save(&input_path).unwrap();

        let template = crate::template::load_dir("sym", &dir).unwrap();
        let mut request = Request::default();
        request
            .images
            .insert("to".to_string(), input_path.to_string_lossy().to_string());

        let assets = AssetStore::new(8);
        let font = crate::text::FontSet::detect("");
        let output = block_on(render(
            &template,
            &request,
            &assets,
            font.as_ref(),
            60,
            4096,
        ))
        .unwrap();
        let decoded = image::load_from_memory(&output.bytes).unwrap().to_rgba8();

        assert_eq!(decoded.dimensions(), (200, 100));
        for y in 0..100u32 {
            for x in 0..100u32 {
                let base = decoded.get_pixel(x, y).0;
                assert_eq!(
                    base,
                    decoded.get_pixel(199 - x, y).0,
                    "左右镜像(x={x},y={y})"
                );
                assert_eq!(
                    base,
                    decoded.get_pixel(x, 99 - y).0,
                    "上下镜像(x={x},y={y})"
                );
            }
        }
    }
    #[test]
    fn renders_static_png_and_substitutes_defaults() {
        let dir = scratch("png");
        write_png(&dir.join("bg.png"), 64, 48, [255, 255, 255, 255]);
        std::fs::write(
            dir.join("template.json"),
            r##"{
              "type": "image",
              "canvas": {"width": 64, "height": 48},
              "elements": [
                {"type": "background", "src": "bg.png"},
                {"type": "text", "text": "${missing:-fallback}", "coords": [4, 24], "size": 12}
              ]
            }"##,
        )
        .unwrap();

        let template = crate::template::load_dir("plain", &dir).unwrap();
        assert_eq!(template.kind, Kind::Image);

        let mut request = Request::default();
        assert_eq!(substitute("${missing:-fallback}", &request), "fallback");
        request.texts.insert("to".to_string(), "浣熊".to_string());
        assert_eq!(substitute("x=${to}", &request), "x=浣熊");
        // 旧模板 `$txt1[这个]` 会转成 `${0:这个}`，冒号后是默认值
        assert_eq!(substitute("${0:这个}", &request), "这个");
        assert_eq!(substitute("${to:=浣熊二号}", &request), "浣熊");
        assert_eq!(substitute("${outer:${to}}", &request), "浣熊");
        assert_eq!(substitute(r"a\$b", &request), "a$b");

        let assets = AssetStore::new(4);
        let output = block_on(render(&template, &request, &assets, None, 999, 4096)).unwrap();
        assert_eq!(output.extension, "png");
        assert_eq!(output.frames, 1);
        assert!(output.bytes.starts_with(&[0x89, b'P', b'N', b'G']));

        let decoded = image::load_from_memory(&output.bytes).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (64, 48));
    }

    #[test]
    fn renders_p4a_deform_template() {
        let dir = scratch("p4a");
        write_png(&dir.join("bg.png"), 120, 120, [255, 255, 255, 255]);
        write_png(&dir.join("avatar.png"), 40, 40, [220, 30, 30, 255]);
        std::fs::write(
            dir.join("template.json"),
            r##"{
              "type": "image",
              "canvas": {"width": 120, "height": 120},
              "elements": [
                {"type": "background", "src": "bg.png"},
                {"type": "image", "key": "to",
                 "coords": [[10, 10], [110, 30], [100, 110], [20, 100], [30, 30]],
                 "fit": "fill"}
              ]
            }"##,
        )
        .unwrap();

        let template = crate::template::load_dir("p4a", &dir).unwrap();
        let mut request = Request::default();
        request.images.insert(
            "to".to_string(),
            dir.join("avatar.png").to_string_lossy().to_string(),
        );

        let assets = AssetStore::new(4);
        let output = block_on(render(&template, &request, &assets, None, 10, 4096)).unwrap();
        let decoded = image::load_from_memory(&output.bytes).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (120, 120));
        assert!(
            changed_pixels(&decoded, [255, 255, 255, 255]) > 300,
            "P4A 透视变形应画出像素"
        );
    }

    /// P4A 必须把整张源图铺到四边形上：源图四象限各自的颜色都该出现在成品里。
    /// 之前 `rect_matrix` 忘了求逆，映射退化，成品只剩两片被钳到边角的纯色三角。
    #[test]
    fn p4a_deform_maps_every_source_quadrant() {
        let dir = scratch("p4a_quad");
        write_png(&dir.join("bg.png"), 120, 120, [255, 255, 255, 255]);
        let quadrants: [[u8; 4]; 4] = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 0, 255],
        ];
        let mut avatar = RgbaImage::new(40, 40);
        for (x, y, pixel) in avatar.enumerate_pixels_mut() {
            let index = usize::from(x >= 20) + usize::from(y >= 20) * 2;
            *pixel = Rgba(quadrants[index]);
        }
        avatar.save(dir.join("avatar.png")).unwrap();
        std::fs::write(
            dir.join("template.json"),
            r##"{
              "type": "image",
              "canvas": {"width": 120, "height": 120},
              "elements": [
                {"type": "background", "src": "bg.png"},
                {"type": "image", "key": "to",
                 "coords": [[20, 20], [100, 20], [100, 100], [20, 100], [20, 20]],
                 "fit": "fill"}
              ]
            }"##,
        )
        .unwrap();

        let template = crate::template::load_dir("p4a_quad", &dir).unwrap();
        let mut request = Request::default();
        request.images.insert(
            "to".to_string(),
            dir.join("avatar.png").to_string_lossy().to_string(),
        );
        let assets = AssetStore::new(4);
        let output = block_on(render(&template, &request, &assets, None, 10, 4096)).unwrap();
        let decoded = image::load_from_memory(&output.bytes).unwrap().to_rgba8();

        let close =
            |pixel: &Rgba<u8>, target: [u8; 4]| (0..3).all(|i| pixel.0[i].abs_diff(target[i]) < 40);
        let mut counts = [0usize; 4];
        for pixel in decoded.pixels() {
            for (index, target) in quadrants.iter().enumerate() {
                if close(pixel, *target) {
                    counts[index] += 1;
                }
            }
        }
        for (index, count) in counts.iter().enumerate() {
            assert!(
                *count > 500,
                "源图第 {index} 象限没铺到成品上（counts={counts:?}）"
            );
        }
    }

    #[test]
    fn renders_filter_chain_with_mirage_and_crop() {
        let dir = scratch("filters");
        write_png(&dir.join("bg.png"), 80, 80, [255, 255, 255, 255]);
        write_png(&dir.join("avatar.png"), 60, 60, [210, 70, 20, 255]);
        write_png(&dir.join("inner.png"), 60, 60, [20, 90, 210, 255]);
        std::fs::write(
            dir.join("template.json"),
            r##"{
              "type": "image",
              "canvas": {"width": 80, "height": 80},
              "elements": [
                {"type": "background", "src": "bg.png"},
                {"type": "image", "key": "to", "coords": [10, 10, 60, 60],
                 "crop": [[0, 0], ["100%", 0], ["100%", "100%"], [0, "100%"]],
                 "filter": ["gray", {"type": "blur", "radius": 2},
                            {"type": "mirage", "src": "inner.png"}]}
              ]
            }"##,
        )
        .unwrap();

        let template = crate::template::load_dir("filters", &dir).unwrap();
        let mut request = Request::default();
        request.images.insert(
            "to".to_string(),
            dir.join("avatar.png").to_string_lossy().to_string(),
        );

        let assets = AssetStore::new(4);
        let output = block_on(render(&template, &request, &assets, None, 10, 4096)).unwrap();
        assert!(output.bytes.starts_with(&[0x89, b'P', b'N', b'G']));
        let decoded = image::load_from_memory(&output.bytes).unwrap().to_rgba8();
        assert!(
            changed_pixels(&decoded, [255, 255, 255, 255]) > 200,
            "滤镜链应画出像素"
        );
    }

    /// 遍历整个模板目录逐个渲染，确认解析/渲染不崩（缺素材的模板会画成空白，不算失败）
    #[test]
    #[ignore = "需要外部模板目录，用 PETPET_TEMPLATES_DIR 指定后手动运行"]
    fn renders_every_template_in_dir() {
        let Ok(root) = std::env::var("PETPET_TEMPLATES_DIR") else {
            return;
        };
        let root = PathBuf::from(root);
        if !root.is_dir() {
            return;
        }
        let sample = std::env::var("PETPET_SAMPLE_IMAGE").ok();
        let assets = AssetStore::new(64);
        let font = crate::text::FontSet::detect("");
        let mut ok = 0usize;
        let mut failed: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&root).unwrap().flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let id = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            let template = match crate::template::load_dir(&id, &path) {
                Ok(template) => template,
                Err(err) => {
                    failed.push(format!("{id}: 解析失败 {err}"));
                    continue;
                }
            };
            let mut request = Request::default();
            if let Some(sample) = &sample {
                for key in &template.image_keys {
                    request.images.insert(key.clone(), sample.clone());
                }
            }
            for key in &template.text_keys {
                request.texts.insert(key.clone(), "这个".to_string());
            }
            request.texts.insert("0".to_string(), "这个".to_string());
            match block_on(render(
                &template,
                &request,
                &assets,
                font.as_ref(),
                60,
                1024,
            )) {
                Ok(_) => ok += 1,
                Err(err) => failed.push(format!("{id}: 渲染失败 {err}")),
            }
        }
        eprintln!("成功 {ok} 个，失败 {} 个", failed.len());
        for line in &failed {
            eprintln!("  {line}");
        }
        assert!(ok > 0, "至少应渲染成功一个模板");
    }

    /// 用真实 petpet 模板目录冒烟：`PETPET_TEMPLATES_DIR=<模板目录> cargo test -- --ignored`
    /// 目录形如 `.../test-templates/template`（同级需有它引用的素材）。
    #[test]
    #[ignore = "需要外部模板素材，用 PETPET_TEMPLATES_DIR 指定后手动运行"]
    fn renders_real_template_dir_smoke() {
        let Ok(root) = std::env::var("PETPET_TEMPLATES_DIR") else {
            return;
        };
        let root = PathBuf::from(root);
        if !root.is_dir() {
            return;
        }
        let id = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("template");
        let template = crate::template::load_dir(id, &root).unwrap();

        let mut request = Request::default();
        if let Ok(sample) = std::env::var("PETPET_SAMPLE_IMAGE") {
            for key in &template.image_keys {
                request.images.insert(key.clone(), sample.clone());
            }
        }
        for key in &template.text_keys {
            request.texts.insert(key.clone(), "这个".to_string());
        }

        request.texts.insert("0".to_string(), "这个".to_string());

        let assets = AssetStore::new(8);
        let font = crate::text::FontSet::detect("");
        let output = block_on(render(
            &template,
            &request,
            &assets,
            font.as_ref(),
            120,
            4096,
        ))
        .unwrap();
        eprintln!(
            "真实模板 {id}: {} 帧, {} 字节, 扩展名 {}",
            output.frames,
            output.bytes.len(),
            output.extension
        );
        let dump = std::env::temp_dir().join(format!("petpet_smoke_render.{}", output.extension));
        std::fs::write(&dump, &output.bytes).unwrap();
        eprintln!("dumped to {}", dump.display());
        assert!(output.bytes.len() > 200);
    }
}

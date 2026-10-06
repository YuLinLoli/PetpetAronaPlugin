//! 图像操作与滤镜（缩放/裁切/圆角/旋转/混合），供 [`crate::render`] 使用。

use image::{imageops::FilterType, Rgba, RgbaImage};

use crate::length::Offset;
use crate::template::{Fit, Filter, MirageSpec, RadiusSpec};

/// 按 fit 把源图放进 target_w × target_h 的框里；`position` 决定内容在框内的偏移
pub fn fit_into(
    src: &RgbaImage,
    target_w: u32,
    target_h: u32,
    fit: Fit,
    position: &Offset,
) -> RgbaImage {
    let (sw, sh) = src.dimensions();
    if target_w == 0 || target_h == 0 {
        return RgbaImage::new(target_w.max(1), target_h.max(1));
    }
    match fit {
        Fit::Fill => resize(src, target_w, target_h),
        Fit::Contain => {
            let scale = (target_w as f32 / sw as f32).min(target_h as f32 / sh as f32);
            let (cw, ch) = (
                (sw as f32 * scale).round().max(1.0) as u32,
                (sh as f32 * scale).round().max(1.0) as u32,
            );
            let scaled = resize(src, cw, ch);
            let mut canvas = RgbaImage::new(target_w, target_h);
            let (ox, oy) = position.eval(target_w as f32 - cw as f32, target_h as f32 - ch as f32);
            paste(&mut canvas, &scaled, ox.round() as i32, oy.round() as i32, 1.0);
            canvas
        }
        Fit::Cover => {
            let scale = (target_w as f32 / sw as f32).max(target_h as f32 / sh as f32);
            let cw = (sw as f32 * scale).ceil().max(1.0) as u32;
            let ch = (sh as f32 * scale).ceil().max(1.0) as u32;
            let scaled = resize(src, cw, ch);
            let mut canvas = RgbaImage::new(target_w, target_h);
            let (ox, oy) = position.eval(target_w as f32 - cw as f32, target_h as f32 - ch as f32);
            paste(&mut canvas, &scaled, ox.round() as i32, oy.round() as i32, 1.0);
            canvas
        }
        Fit::None => {
            let mut canvas = RgbaImage::new(target_w, target_h);
            let (ox, oy) = position.eval(target_w as f32 - sw as f32, target_h as f32 - sh as f32);
            paste(&mut canvas, src, ox.round() as i32, oy.round() as i32, 1.0);
            canvas
        }
    }
}

/// 把源图缩放到指定尺寸（三角形插值，接近 petpet 的重采样观感）
pub fn resize(src: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    if src.width() == w && src.height() == h {
        return src.clone();
    }
    image::imageops::resize(src, w.max(1), h.max(1), FilterType::Triangle)
}

/// 裁切（越界部分用透明填充）
pub fn crop(src: &RgbaImage, x: i32, y: i32, w: u32, h: u32) -> RgbaImage {
    if w == 0 || h == 0 {
        return RgbaImage::new(w.max(1), h.max(1));
    }
    let mut out = RgbaImage::new(w, h);
    for oy in 0..h as i32 {
        for ox in 0..w as i32 {
            let sx = x + ox;
            let sy = y + oy;
            if sx >= 0 && sy >= 0 && (sx as u32) < src.width() && (sy as u32) < src.height() {
                out.put_pixel(ox as u32, oy as u32, *src.get_pixel(sx as u32, sy as u32));
            }
        }
    }
    out
}

/// 把 src 叠到 dst 的 (x, y)，global 为整体不透明度
pub fn paste(dst: &mut RgbaImage, src: &RgbaImage, x: i32, y: i32, global: f32) {
    let global = global.clamp(0.0, 1.0);
    if global <= 0.0 {
        return;
    }
    for sy in 0..src.height() as i32 {
        for sx in 0..src.width() as i32 {
            let dx = x + sx;
            let dy = y + sy;
            if dx < 0 || dy < 0 || dx >= dst.width() as i32 || dy >= dst.height() as i32 {
                continue;
            }
            let pixel = *src.get_pixel(sx as u32, sy as u32);
            blend_over(dst, dx as u32, dy as u32, pixel, global);
        }
    }
}

/// 普通 source-over 混合
pub fn blend_over(dst: &mut RgbaImage, x: u32, y: u32, src: Rgba<u8>, global: f32) {
    let sa = src[3] as f32 / 255.0 * global;
    if sa <= 0.0 {
        return;
    }
    let target = dst.get_pixel_mut(x, y);
    let da = target[3] as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 0.0 {
        *target = Rgba([0, 0, 0, 0]);
        return;
    }
    let mix = |s: u8, d: u8| -> u8 {
        let value = (s as f32 * sa + d as f32 * da * (1.0 - sa)) / out_a;
        value.clamp(0.0, 255.0) as u8
    };
    *target = Rgba([
        mix(src[0], target[0]),
        mix(src[1], target[1]),
        mix(src[2], target[2]),
        (out_a * 255.0).clamp(0.0, 255.0) as u8,
    ]);
}

/// 按 4 个角半径做圆角遮罩（单位 px，顺序为左上、右上、右下、左下）
pub fn round_corners(image: &mut RgbaImage, radii: [f32; 4]) {
    let (w, h) = (image.width() as f32, image.height() as f32);
    let clamp = |r: f32| r.clamp(0.0, w.min(h) / 2.0);
    let [tl, tr, br, bl] = [
        clamp(radii[0]),
        clamp(radii[1]),
        clamp(radii[2]),
        clamp(radii[3]),
    ];
    if tl <= 0.0 && tr <= 0.0 && br <= 0.0 && bl <= 0.0 {
        return;
    }
    for y in 0..image.height() {
        for x in 0..image.width() {
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            let (cx, cy, r) = if fx < tl && fy < tl {
                (tl, tl, tl)
            } else if fx > w - tr && fy < tr {
                (w - tr, tr, tr)
            } else if fx > w - br && fy > h - br {
                (w - br, h - br, br)
            } else if fx < bl && fy > h - bl {
                (bl, h - bl, bl)
            } else {
                continue;
            };
            if r <= 0.0 {
                continue;
            }
            let dx = fx - cx;
            let dy = fy - cy;
            if dx * dx + dy * dy > r * r {
                let pixel = image.get_pixel_mut(x, y);
                pixel[3] = 0;
            }
        }
    }
}

/// 绕 `origin`（相对图像尺寸的偏移）顺时针旋转 `angle` 度
pub fn rotate(image: &RgbaImage, angle: f32, origin: &Offset) -> RgbaImage {
    rotate_about(image, angle, origin).0
}

/// 同 [`rotate`]，另外返回旋转后画布左上角相对原图左上角的偏移 `(min_x, min_y)`：
/// 旋转中心在输出图内的位置是 `(ox - min_x, oy - min_y)`。
pub fn rotate_about(image: &RgbaImage, angle: f32, origin: &Offset) -> (RgbaImage, f32, f32) {
    let (w, h) = (image.width() as f32, image.height() as f32);
    let (ox, oy) = origin.eval(w, h);
    rotate_about_px(image, angle, ox, oy)
}

/// 同 [`rotate_about`]，但直接传入旋转中心的像素偏移 `(ox, oy)`
pub fn rotate_about_px(image: &RgbaImage, angle: f32, ox: f32, oy: f32) -> (RgbaImage, f32, f32) {
    let (w, h) = (image.width() as f32, image.height() as f32);
    if angle.abs() < 0.01 || w < 1.0 || h < 1.0 {
        return (image.clone(), 0.0, 0.0);
    }
    let rad = angle.to_radians();
    let (sin, cos) = rad.sin_cos();
    // 正向：p -> R*(p - o) + o
    let forward = |x: f32, y: f32| -> (f32, f32) {
        let dx = x - ox;
        let dy = y - oy;
        (cos * dx - sin * dy + ox, sin * dx + cos * dy + oy)
    };
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (x, y) in [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)] {
        let (rx, ry) = forward(x, y);
        min_x = min_x.min(rx);
        min_y = min_y.min(ry);
        max_x = max_x.max(rx);
        max_y = max_y.max(ry);
    }
    let out_w = (max_x - min_x).ceil().max(1.0) as u32;
    let out_h = (max_y - min_y).ceil().max(1.0) as u32;
    let mut out = RgbaImage::new(out_w, out_h);
    for dy in 0..out_h {
        for dx in 0..out_w {
            // 逆变换：dest 坐标 + (min) 先在"正向空间"，再逆旋转回源
            let px = dx as f32 + 0.5 + min_x;
            let py = dy as f32 + 0.5 + min_y;
            let bx = px - ox;
            let by = py - oy;
            let sx = cos * bx + sin * by + ox;
            let sy = -sin * bx + cos * by + oy;
            if sx < 0.0 || sy < 0.0 || sx > w || sy > h {
                continue;
            }
            if let Some(pixel) = sample_bilinear(image, sx, sy) {
                out.put_pixel(dx, dy, pixel);
            }
        }
    }
    (out, min_x, min_y)
}

fn sample_bilinear(image: &RgbaImage, x: f32, y: f32) -> Option<Rgba<u8>> {
    let (w, h) = (image.width() as i32, image.height() as i32);
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    if x0 < -1 || y0 < -1 || x0 > w || y0 > h {
        return None;
    }
    let mut acc = [0.0f32; 4];
    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let px = x0 + dx;
        let py = y0 + dy;
        if px < 0 || py < 0 || px >= w || py >= h {
            continue;
        }
        let weight = ((1.0 - (x - px as f32).abs()).max(0.0)) * ((1.0 - (y - py as f32).abs()).max(0.0));
        let pixel = image.get_pixel(px as u32, py as u32);
        for c in 0..4 {
            acc[c] += pixel[c] as f32 * weight;
        }
    }
    Some(Rgba([
        acc[0].clamp(0.0, 255.0) as u8,
        acc[1].clamp(0.0, 255.0) as u8,
        acc[2].clamp(0.0, 255.0) as u8,
        acc[3].clamp(0.0, 255.0) as u8,
    ]))
}

/// 圆角半径求值
pub fn radius_values(spec: &RadiusSpec, w: u32, h: u32, ctx: &crate::length::Ctx) -> [f32; 4] {
    let eval = |node: &crate::length::Node| node_value(node, ctx, w, h);
    match spec {
        RadiusSpec::None => [0.0; 4],
        RadiusSpec::Uniform(node) => {
            let v = eval(node);
            [v; 4]
        }
        RadiusSpec::Corners(nodes) => [
            eval(&nodes[0]),
            eval(&nodes[1]),
            eval(&nodes[2]),
            eval(&nodes[3]),
        ],
    }
}

fn node_value(node: &crate::length::Node, ctx: &crate::length::Ctx, w: u32, h: u32) -> f32 {
    use crate::length::{Axis, Ctx};
    let mut local = Ctx::new(w as f32, h as f32);
    local.vars = ctx.vars.clone();
    local.eval(node, Axis::X)
}

/// 滤镜渲染需要的外部素材：按 `filters` 里 `Mirage` 出现的顺序给出「内层图」
#[derive(Default, Clone)]
pub struct FilterAssets {
    pub mirages: Vec<Option<RgbaImage>>,
    /// 当前帧号（带动画的滤镜用）
    pub frame: usize,
}

/// 应用滤镜链
pub fn apply_filters(image: &mut RgbaImage, filters: &[Filter], assets: &FilterAssets) {
    let frame = assets.frame;
    let mut mirage_index = 0usize;
    for filter in filters {
        match filter {
            Filter::Gray => gray(image),
            Filter::Binarize => binarize(image),
            Filter::Mirror => *image = image::imageops::flip_horizontal(image),
            Filter::Flip => *image = image::imageops::flip_vertical(image),
            Filter::Blur(radius) => blur(image, *radius),
            Filter::Contrast {
                brightness,
                contrast,
            } => apply_contrast(image, *brightness, *contrast),
            Filter::Hsb {
                hue,
                saturation,
                brightness,
            } => hsb(image, *hue, *saturation, *brightness),
            Filter::Noise(amount) => noise(image, *amount),
            Filter::Swirl { radius, angle, x, y } => {
                *image = swirl(image, *radius, *angle / 2.0, *x, *y)
            }
            Filter::Bulge {
                radius,
                strength,
                x,
                y,
            } => *image = bulge(image, *radius, *strength, *x, *y),
            Filter::Swim {
                scale,
                stretch,
                angle,
                amount,
                turbulence,
                time,
            } => *image = swim(image, *scale, *stretch, *angle, *amount, *turbulence, *time),
            Filter::Halftone { radius, angle } => {
                *image = color_halftone(image, *radius, *angle, *angle, *angle)
            }
            Filter::DotScreen { radius, angle } => {
                let mut out = color_halftone(image, *radius, *angle, *angle, *angle);
                gray(&mut out);
                *image = out;
            }
            Filter::Denoise => *image = denoise(image),
            Filter::Oil {
                skip,
                range,
                levels,
            } => *image = oil(image, *skip, *range, *levels),
            Filter::SwingTwist {
                max_angle,
                amplitude,
                omega,
                decay,
                index_offset,
            } => {
                *image = swing_twist(
                    image,
                    *max_angle,
                    *amplitude,
                    *omega,
                    *decay,
                    frame as i32 + *index_offset,
                )
            }
            Filter::Mirage(spec) => {
                let inner = assets
                    .mirages
                    .get(mirage_index)
                    .and_then(|item| item.as_ref());
                mirage_index += 1;
                if let Some(inner) = inner {
                    *image = mirage(image, inner, spec);
                }
            }
        }
    }
}
fn gray(image: &mut RgbaImage) {
    for pixel in image.pixels_mut() {
        let value = (0.299 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.114 * pixel[2] as f32)
            .round()
            .clamp(0.0, 255.0) as u8;
        pixel[0] = value;
        pixel[1] = value;
        pixel[2] = value;
    }
}

fn binarize(image: &mut RgbaImage) {
    for pixel in image.pixels_mut() {
        let luminance =
            0.299 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.114 * pixel[2] as f32;
        let value = if luminance >= 128.0 { 255 } else { 0 };
        pixel[0] = value;
        pixel[1] = value;
        pixel[2] = value;
    }
}

/// 对齐 jhlabs 的 `ContrastFilter`（petpet 把 brightness / contrast 各 +1 后传进来）
fn apply_contrast(image: &mut RgbaImage, brightness: f32, contrast: f32) {
    let brightness = brightness + 1.0;
    let contrast = contrast + 1.0;
    for pixel in image.pixels_mut() {
        for c in 0..3 {
            let mut value = pixel[c] as f32 * brightness + 0.5;
            value = (value - 127.0) * contrast + 127.5;
            pixel[c] = value.clamp(0.0, 255.0) as u8;
        }
    }
}

/// 对齐 jhlabs 的 `HSBAdjustFilter`（hue 为弧度）
fn hsb(image: &mut RgbaImage, hue: f32, saturation: f32, brightness: f32) {
    for pixel in image.pixels_mut() {
        let (h, s, b) = rgb_to_hsb(pixel[0], pixel[1], pixel[2]);
        let mut next_hue = h / 360.0 + hue;
        next_hue -= next_hue.floor();
        let (r, g, bl) = hsb_to_rgb(
            next_hue * 360.0,
            (s + saturation).clamp(0.0, 1.0),
            (b + brightness).clamp(0.0, 1.0),
        );
        pixel[0] = r;
        pixel[1] = g;
        pixel[2] = bl;
    }
}

fn rgb_to_hsb(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let hue = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let saturation = if max == 0.0 { 0.0 } else { delta / max };
    (hue.rem_euclid(360.0), saturation, max)
}

fn hsb_to_rgb(hue: f32, saturation: f32, brightness: f32) -> (u8, u8, u8) {
    let c = brightness * saturation;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = brightness - c;
    let (r, g, b) = match (hue.rem_euclid(360.0) / 60.0) as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0).clamp(0.0, 255.0) as u8,
        ((g + m) * 255.0).clamp(0.0, 255.0) as u8,
        ((b + m) * 255.0).clamp(0.0, 255.0) as u8,
    )
}

/// 对齐 jhlabs 的 `BoxBlurFilter`：横向 / 纵向各 3 遍箱式模糊
fn blur(image: &mut RgbaImage, radius: f32) {
    let radius = radius.max(0.0);
    if radius < 1.0 {
        return;
    }
    let r = radius.round() as i64;
    let (w, h) = (image.width(), image.height());
    let mut pixels: Vec<[f32; 4]> = image
        .pixels()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32])
        .collect();
    for _ in 0..3 {
        pixels = box_blur(&pixels, w, h, r, true);
        pixels = box_blur(&pixels, w, h, r, false);
    }
    for (index, pixel) in image.pixels_mut().enumerate() {
        for c in 0..4 {
            pixel[c] = pixels[index][c].clamp(0.0, 255.0) as u8;
        }
    }
}

fn box_blur(pixels: &[[f32; 4]], w: u32, h: u32, r: i64, horizontal: bool) -> Vec<[f32; 4]> {
    let mut out = vec![[0.0f32; 4]; pixels.len()];
    let table = (2 * r + 1) as f32;
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 4];
            for d in -r..=r {
                let (sx, sy) = if horizontal {
                    ((x as i64 + d).clamp(0, w as i64 - 1) as u32, y)
                } else {
                    (x, (y as i64 + d).clamp(0, h as i64 - 1) as u32)
                };
                let pixel = pixels[(sy * w + sx) as usize];
                for c in 0..4 {
                    acc[c] += pixel[c];
                }
            }
            for c in 0..4 {
                out[(y * w + x) as usize][c] = acc[c] / table;
            }
        }
    }
    out
}

/// 对齐 jhlabs 的 `NoiseFilter`（amount 为 0..1，换算成 0..100 的灰度扰动）
fn noise(image: &mut RgbaImage, amount: f32) {
    let amount = amount * 100.0;
    let mut seed = 0x9E37_79B9u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed % 2001) as f32 - 1000.0
    };
    for pixel in image.pixels_mut() {
        let delta = next() / 1000.0 * amount;
        for c in 0..3 {
            pixel[c] = (pixel[c] as f32 + delta).clamp(0.0, 255.0) as u8;
        }
    }
}
/// 双线性采样（越界按边缘钳制）
fn sample_clamp(image: &RgbaImage, x: f32, y: f32) -> Rgba<u8> {
    let (w, h) = (image.width() as i64, image.height() as i64);
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let mut acc = [0.0f32; 4];
    for (dx, dy) in [(0i64, 0i64), (1, 0), (0, 1), (1, 1)] {
        let px = (x0 + dx).clamp(0, w - 1) as u32;
        let py = (y0 + dy).clamp(0, h - 1) as u32;
        let weight =
            (1.0 - (x - px as f32).abs()).max(0.0) * (1.0 - (y - py as f32).abs()).max(0.0);
        let pixel = image.get_pixel(px, py);
        for c in 0..4 {
            acc[c] += pixel[c] as f32 * weight;
        }
    }
    Rgba([
        acc[0].clamp(0.0, 255.0) as u8,
        acc[1].clamp(0.0, 255.0) as u8,
        acc[2].clamp(0.0, 255.0) as u8,
        acc[3].clamp(0.0, 255.0) as u8,
    ])
}

/// 逆变换扭曲的公共骨架：`inverse(x, y) -> (src_x, src_y)`；`clamp_edge` 为假时越界留透明
fn distort(
    image: &RgbaImage,
    clamp_edge: bool,
    inverse: impl Fn(f32, f32) -> (f32, f32),
) -> RgbaImage {
    let (w, h) = image.dimensions();
    let mut out = RgbaImage::new(w.max(1), h.max(1));
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = inverse(x as f32, y as f32);
            let inside = clamp_edge
                || (sx >= 0.0
                    && sy >= 0.0
                    && sx <= (w - 1) as f32
                    && sy <= (h - 1) as f32);
            if inside {
                out.put_pixel(x, y, sample_clamp(image, sx, sy));
            }
        }
    }
    out
}

/// 对齐 jhlabs 的 `TwirlFilter`
fn swirl(image: &RgbaImage, radius: f32, angle: f32, cx: f32, cy: f32) -> RgbaImage {
    let (w, h) = (image.width() as f32, image.height() as f32);
    let icx = w * cx;
    let icy = h * cy;
    let radius = if radius == 0.0 { icx.min(icy) } else { radius };
    if radius <= 0.0 || angle == 0.0 {
        return image.clone();
    }
    let radius2 = radius * radius;
    distort(image, true, |x, y| {
        let dx = x - icx;
        let dy = y - icy;
        let distance2 = dx * dx + dy * dy;
        if distance2 > radius2 {
            (x, y)
        } else {
            let distance = distance2.sqrt();
            let a = dy.atan2(dx) + angle * (radius - distance) / radius;
            (icx + distance * a.cos(), icy + distance * a.sin())
        }
    })
}

/// 对齐 petpet 的 `PetpetBulgePinchFilter`（最近邻，与原实现一致）
fn bulge(image: &RgbaImage, radius: f32, strength: f32, cx: f32, cy: f32) -> RgbaImage {
    if strength == 0.0 {
        return image.clone();
    }
    let (w, h) = image.dimensions();
    let center_x = (w as f32 * cx) as i32;
    let center_y = (h as f32 * cy) as i32;
    let radius = if radius == 0.0 {
        center_x.min(center_y) as f32
    } else {
        radius
    };
    if radius <= 0.0 {
        return image.clone();
    }
    let radius2 = radius * radius;
    let is_bulge = strength > 0.0;
    let mut out = RgbaImage::new(w, h);
    for fy in 0..h as i32 {
        for fx in 0..w as i32 {
            let mut dx = (fx - center_x) as f32;
            let mut dy = (fy - center_y) as f32;
            let distance2 = dx * dx + dy * dy;
            let (mut sx, mut sy) = (fx, fy);
            if distance2 < radius2 && distance2 > 0.0 {
                if is_bulge {
                    let distance = distance2.sqrt();
                    let alpha = 1.0 - distance / radius;
                    let factor = alpha.powf(1.0 / strength) * distance;
                    sx -= (factor * dx / distance) as i32;
                    sy -= (factor * dy / distance) as i32;
                } else {
                    let d = (distance2 / radius2).sqrt();
                    let t = (std::f32::consts::PI * 0.5 * d).sin().powf(strength);
                    dx *= t;
                    dy *= t;
                    sx = center_x + dx as i32;
                    sy = center_y + dy as i32;
                }
            }
            if sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 {
                let pixel = *image.get_pixel(sx as u32, sy as u32);
                out.put_pixel(fx as u32, fy as u32, pixel);
            }
        }
    }
    out
}

/// 对齐 jhlabs 的 `SwimFilter`（Perlin 噪声扰动）
fn swim(
    image: &RgbaImage,
    scale: f32,
    stretch: f32,
    angle: f32,
    amount: f32,
    turbulence: f32,
    time: f32,
) -> RgbaImage {
    let (sin, cos) = angle.sin_cos();
    let scale = if scale == 0.0 { 1.0 } else { scale };
    let stretch = if stretch == 0.0 { 1.0 } else { stretch };
    distort(image, false, |x, y| {
        let nx = (cos * x + sin * y) / scale;
        let ny = (-sin * x + cos * y) / (scale * stretch);
        let (ox, oy) = if (turbulence - 1.0).abs() < f32::EPSILON {
            (
                perlin_noise3(nx + 0.5, ny, time),
                perlin_noise3(nx, ny + 0.5, time),
            )
        } else {
            (
                perlin_turbulence3(nx + 0.5, ny, time, turbulence),
                perlin_turbulence3(nx, ny + 0.5, time, turbulence),
            )
        };
        (x + amount * ox, y + amount * oy)
    })
}

fn mod_positive(value: f32, modulus: f32) -> f32 {
    let r = value % modulus;
    if r < 0.0 { r + modulus } else { r }
}

fn smooth_step(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 <= edge0 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 对齐 jhlabs 的 `ColorHalftoneFilter`
fn color_halftone(
    image: &RgbaImage,
    dot_radius: f32,
    cyan: f32,
    magenta: f32,
    yellow: f32,
) -> RgbaImage {
    let (w, h) = image.dimensions();
    let grid = 2.0 * dot_radius * 1.414;
    if grid <= 0.0 {
        return image.clone();
    }
    let half = grid / 2.0;
    let angles = [
        cyan.to_radians() + 108f32.to_radians(),
        magenta.to_radians() + 162f32.to_radians(),
        yellow.to_radians() + 90f32.to_radians(),
    ];
    let mx = [0.0f32, -1.0, 1.0, 0.0, 0.0];
    let my = [0.0f32, 0.0, 0.0, -1.0, 1.0];
    let mut out = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut channels = [255.0f32; 3];
            for (channel, angle) in angles.iter().enumerate() {
                let (sin, cos) = angle.sin_cos();
                let tx = x as f32 * cos + y as f32 * sin;
                let ty = -(x as f32) * sin + y as f32 * cos;
                let tx = tx - mod_positive(tx - half, grid) + half;
                let ty = ty - mod_positive(ty - half, grid) + half;
                let mut f = 1.0f32;
                for i in 0..5 {
                    let ttx = tx + mx[i] * grid;
                    let tty = ty + my[i] * grid;
                    let ntx = ttx * cos - tty * sin;
                    let nty = ttx * sin + tty * cos;
                    let nx = (ntx as i32).clamp(0, w as i32 - 1) as u32;
                    let ny = (nty as i32).clamp(0, h as i32 - 1) as u32;
                    let pixel = image.get_pixel(nx, ny);
                    let value = pixel[channel] as f32 / 255.0;
                    let l = (1.0 - value * value) * half * 1.414;
                    let dx = x as f32 - ntx;
                    let dy = y as f32 - nty;
                    let r = (dx * dx + dy * dy).sqrt();
                    let f2 = 1.0 - smooth_step(r, r + 1.0, l);
                    f = f.min(f2);
                }
                channels[channel] = 255.0 * f;
            }
            let alpha = image.get_pixel(x, y)[3];
            out.put_pixel(
                x,
                y,
                Rgba([
                    channels[0].clamp(0.0, 255.0) as u8,
                    channels[1].clamp(0.0, 255.0) as u8,
                    channels[2].clamp(0.0, 255.0) as u8,
                    alpha,
                ]),
            );
        }
    }
    out
}

/// 对齐 jhlabs 的 `MedianFilter`
fn denoise(image: &RgbaImage) -> RgbaImage {
    let (w, h) = image.dimensions();
    let mut out = RgbaImage::new(w, h);
    let mut argb = [Rgba([0u8, 0, 0, 0]); 9];
    let mut r = [0i32; 9];
    let mut g = [0i32; 9];
    let mut b = [0i32; 9];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut count = 0usize;
            for dy in -1i32..=1 {
                let iy = y + dy;
                if iy < 0 || iy >= h as i32 {
                    continue;
                }
                for dx in -1i32..=1 {
                    let ix = x + dx;
                    if ix < 0 || ix >= w as i32 {
                        continue;
                    }
                    let pixel = *image.get_pixel(ix as u32, iy as u32);
                    argb[count] = pixel;
                    r[count] = pixel[0] as i32;
                    g[count] = pixel[1] as i32;
                    b[count] = pixel[2] as i32;
                    count += 1;
                }
            }
            while count < 9 {
                argb[count] = Rgba([0, 0, 0, 255]);
                r[count] = 0;
                g[count] = 0;
                b[count] = 0;
                count += 1;
            }
            let mut best = 0usize;
            let mut best_sum = i32::MAX;
            for i in 0..9 {
                let mut sum = 0i32;
                for j in 0..9 {
                    sum += (r[i] - r[j]).abs() + (g[i] - g[j]).abs() + (b[i] - b[j]).abs();
                }
                if sum < best_sum {
                    best_sum = sum;
                    best = i;
                }
            }
            out.put_pixel(x as u32, y as u32, argb[best]);
        }
    }
    out
}
/// 对齐 petpet 的 `PetpetOilFilter`
fn oil(image: &RgbaImage, skip: i32, range: i32, levels: i32) -> RgbaImage {
    let (w, h) = image.dimensions();
    let levels = levels.clamp(1, 256) as usize;
    let skip = skip.max(1);
    let mut out = RgbaImage::new(w, h);
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut r_histogram = vec![0i32; levels];
            let mut g_histogram = vec![0i32; levels];
            let mut b_histogram = vec![0i32; levels];
            let mut r_total = vec![0i32; levels];
            let mut g_total = vec![0i32; levels];
            let mut b_total = vec![0i32; levels];
            let mut row = -range;
            while row <= range {
                let iy = y + row;
                if iy >= 0 && iy < h as i32 {
                    let mut col = -range;
                    while col <= range {
                        let ix = x + col;
                        if ix >= 0 && ix < w as i32 {
                            let pixel = image.get_pixel(ix as u32, iy as u32);
                            let r = pixel[0] as i32;
                            let g = pixel[1] as i32;
                            let b = pixel[2] as i32;
                            let ri = ((r * levels as i32) >> 8).clamp(0, levels as i32 - 1) as usize;
                            let gi = ((g * levels as i32) >> 8).clamp(0, levels as i32 - 1) as usize;
                            let bi = ((b * levels as i32) >> 8).clamp(0, levels as i32 - 1) as usize;
                            r_total[ri] += r;
                            g_total[gi] += g;
                            b_total[bi] += b;
                            r_histogram[ri] += 1;
                            g_histogram[gi] += 1;
                            b_histogram[bi] += 1;
                        }
                        col += skip;
                    }
                }
                row += skip;
            }
            let (mut ri, mut gi, mut bi) = (0usize, 0usize, 0usize);
            for i in 1..levels {
                if r_histogram[i] > r_histogram[ri] {
                    ri = i;
                }
                if g_histogram[i] > g_histogram[gi] {
                    gi = i;
                }
                if b_histogram[i] > b_histogram[bi] {
                    bi = i;
                }
            }
            let r = if r_histogram[ri] > 0 {
                r_total[ri] / r_histogram[ri]
            } else {
                0
            };
            let g = if g_histogram[gi] > 0 {
                g_total[gi] / g_histogram[gi]
            } else {
                0
            };
            let b = if b_histogram[bi] > 0 {
                b_total[bi] / b_histogram[bi]
            } else {
                0
            };
            let alpha = image.get_pixel(x as u32, y as u32)[3];
            out.put_pixel(
                x as u32,
                y as u32,
                Rgba([
                    r.clamp(0, 255) as u8,
                    g.clamp(0, 255) as u8,
                    b.clamp(0, 255) as u8,
                    alpha,
                ]),
            );
        }
    }
    out
}

/// 对齐 petpet 的 `PetpetSwingTwistFilter`：逐行旋转 + 水平摆动
fn swing_twist(
    image: &RgbaImage,
    max_angle: f32,
    amplitude: f32,
    omega: f32,
    decay: f32,
    index: i32,
) -> RgbaImage {
    let (w, h) = image.dimensions();
    if w == 0 || h == 0 {
        return image.clone();
    }
    let amplitude = w as f32 * amplitude;
    let t = index as f32;
    let damping = (-decay * t).exp();
    let offset = amplitude * (omega * t).sin() * damping;
    let angle = max_angle * (omega * t).sin() * damping;
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let mut out = RgbaImage::new(w, h);
    for y in 0..h {
        let factor = 1.0 - y as f32 / h as f32;
        let off = offset * factor;
        let ang = angle * factor;
        let (sin, cos) = ang.sin_cos();
        for x in 0..w {
            let dx = x as f32 - cx - off;
            let dy = y as f32 - cy;
            let sx = cos * dx + sin * dy + cx;
            let sy = -sin * dx + cos * dy + cy;
            if sx >= 0.0 && sy >= 0.0 && sx <= (w - 1) as f32 && sy <= (h - 1) as f32 {
                out.put_pixel(x, y, sample_clamp(image, sx, sy));
            }
        }
    }
    out
}

/// 对齐 petpet 的 `PetpetMirageFilter`：把「内层图」按亮度融进被滤镜的图
fn mirage(cover: &RgbaImage, inner_src: &RgbaImage, spec: &MirageSpec) -> RgbaImage {
    let (iw, ih) = inner_src.dimensions();
    if iw == 0 || ih == 0 {
        return cover.clone();
    }
    let (w, h) = if spec.max_size > 0 {
        let max = spec.max_size as f32;
        if iw > ih {
            (
                spec.max_size as u32,
                ((ih as f32 * max / iw as f32).ceil() as u32).max(1),
            )
        } else {
            (
                ((iw as f32 * max / ih as f32).ceil() as u32).max(1),
                spec.max_size as u32,
            )
        }
    } else {
        (iw, ih)
    };
    let inner = resize(inner_src, w, h);
    let cover = resize(cover, w, h);
    let mut out = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let inner_pixel = inner.get_pixel(x, y);
            let cover_pixel = cover.get_pixel(x, y);
            let inner_gray = 0.299 * inner_pixel[0] as f32
                + 0.587 * inner_pixel[1] as f32
                + 0.114 * inner_pixel[2] as f32;
            let cover_gray = 0.299 * cover_pixel[0] as f32
                + 0.587 * cover_pixel[1] as f32
                + 0.114 * cover_pixel[2] as f32;
            let pixel = if spec.colored {
                let mut inner_data = [0.0f32; 3];
                for c in 0..3 {
                    let scaled = inner_pixel[c] as f32 * spec.inner_scale;
                    let l = inner_gray * spec.inner_scale;
                    inner_data[c] = scaled + (l - scaled) * spec.inner_desat;
                }
                let mut cover_data = [0.0f32; 3];
                for c in 0..3 {
                    let scaled = 255.0 - (255.0 - cover_pixel[c] as f32) * spec.cover_scale;
                    let l = 255.0 - (255.0 - cover_gray) * spec.cover_scale;
                    cover_data[c] = scaled + (l - scaled) * spec.cover_desat;
                }
                let alpha = ((255.0 + inner_gray * spec.inner_scale
                    - (255.0 - (255.0 - cover_gray) * spec.cover_scale))
                    / 255.0)
                    .clamp(0.0, 1.0);
                let ai = 255.0 * alpha;
                let mut channels = [0u8; 3];
                if alpha > 0.0 {
                    for c in 0..3 {
                        let value = ((inner_data[c] - ai + 255.0 - cover_data[c]) * spec.weight + ai
                            - 255.0
                            + cover_data[c])
                            / alpha;
                        channels[c] = value.clamp(0.0, 255.0) as u8;
                    }
                }
                Rgba([
                    channels[0],
                    channels[1],
                    channels[2],
                    ai.clamp(0.0, 255.0) as u8,
                ])
            } else {
                let li = (inner_gray * spec.inner_scale) as i32;
                let lc = (255.0 - (255.0 - cover_gray) * spec.cover_scale) as i32;
                let a = 255 + li - lc;
                if a <= 0 {
                    Rgba([0, 0, 0, 0])
                } else {
                    let l = (li * 255 / a).clamp(0, 255) as u8;
                    Rgba([l, l, l, a.clamp(0, 255) as u8])
                }
            };
            out.put_pixel(x, y, pixel);
        }
    }
    out
}
// ==================== Perlin 噪声（移植 jhlabs Noise） ====================

struct Perlin {
    p: [usize; 514],
    g3: [[f32; 3]; 514],
}

fn perlin() -> &'static Perlin {
    static PERLIN: std::sync::OnceLock<Perlin> = std::sync::OnceLock::new();
    PERLIN.get_or_init(|| {
        let mut seed = 0x2545_F491u32;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed & 0x7fff_ffff) as usize
        };
        let mut p = [0usize; 514];
        let mut g3 = [[0.0f32; 3]; 514];
        for i in 0..256 {
            p[i] = i;
            let mut g = [0.0f32; 3];
            for item in g.iter_mut() {
                *item = (random() % 512) as f32 / 256.0 - 1.0;
            }
            let norm = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
            for j in 0..3 {
                g3[i][j] = if norm > 0.0 { g[j] / norm } else { 0.0 };
            }
        }
        for i in (0..255).rev() {
            let k = p[i];
            let j = random() % 256;
            p[i] = p[j];
            p[j] = k;
        }
        for i in 0..257 {
            p[256 + i] = p[i];
            g3[256 + i] = g3[i];
        }
        Perlin { p, g3 }
    })
}

fn perlin_lerp(t: f32, a: f32, b: f32) -> f32 {
    a + t * (b - a)
}

fn perlin_s_curve(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn perlin_noise3(x: f32, y: f32, z: f32) -> f32 {
    let perlin = perlin();
    let n = 4096.0f32;
    let bx0 = (x + n) as i64 & 0xff;
    let bx1 = (bx0 + 1) & 0xff;
    let rx0 = (x + n) - ((x + n) as i64) as f32;
    let rx1 = rx0 - 1.0;
    let by0 = (y + n) as i64 & 0xff;
    let by1 = (by0 + 1) & 0xff;
    let ry0 = (y + n) - ((y + n) as i64) as f32;
    let ry1 = ry0 - 1.0;
    let bz0 = (z + n) as i64 & 0xff;
    let bz1 = (bz0 + 1) & 0xff;
    let rz0 = (z + n) - ((z + n) as i64) as f32;
    let rz1 = rz0 - 1.0;
    let (bx0, bx1) = (bx0 as usize, bx1 as usize);
    let (by0, by1) = (by0 as usize, by1 as usize);
    let (bz0, bz1) = (bz0 as usize, bz1 as usize);
    let i = perlin.p[bx0];
    let j = perlin.p[bx1];
    let b00 = perlin.p[i + by0];
    let b10 = perlin.p[j + by0];
    let b01 = perlin.p[i + by1];
    let b11 = perlin.p[j + by1];
    let sx = perlin_s_curve(rx0);
    let sy = perlin_s_curve(ry0);
    let sz = perlin_s_curve(rz0);
    let dot = |base: usize, dx: f32, dy: f32, dz: f32| -> f32 {
        let g = perlin.g3[base];
        dx * g[0] + dy * g[1] + dz * g[2]
    };
    let u = dot(b00 + bz0, rx0, ry0, rz0);
    let v = dot(b10 + bz0, rx1, ry0, rz0);
    let a = perlin_lerp(sx, u, v);
    let u = dot(b01 + bz0, rx0, ry1, rz0);
    let v = dot(b11 + bz0, rx1, ry1, rz0);
    let b = perlin_lerp(sx, u, v);
    let c = perlin_lerp(sy, a, b);
    let u = dot(b00 + bz1, rx0, ry0, rz1);
    let v = dot(b10 + bz1, rx1, ry0, rz1);
    let a = perlin_lerp(sx, u, v);
    let u = dot(b01 + bz1, rx0, ry1, rz1);
    let v = dot(b11 + bz1, rx1, ry1, rz1);
    let b = perlin_lerp(sx, u, v);
    let d = perlin_lerp(sy, a, b);
    1.5 * perlin_lerp(sz, c, d)
}

fn perlin_turbulence3(x: f32, y: f32, z: f32, octaves: f32) -> f32 {
    let mut total = 0.0f32;
    let mut f = 1.0f32;
    while f <= octaves {
        total += perlin_noise3(f * x, f * y, f * z).abs() / f;
        f *= 2.0;
    }
    total
}
// ==================== 裁切与透视变形 ====================

/// 矩形裁切：`(x1,y1)-(x2,y2)`，越界部分透明（对齐 petpet 的 `ImageXYWHCropper`）
pub fn crop_rect(src: &RgbaImage, x1: i32, y1: i32, x2: i32, y2: i32) -> RgbaImage {
    let w = (x2 - x1).max(0) as u32;
    let h = (y2 - y1).max(0) as u32;
    if w == 0 || h == 0 {
        return RgbaImage::new(w.max(1), h.max(1));
    }
    let mut out = RgbaImage::new(w, h);
    for oy in 0..h as i32 {
        for ox in 0..w as i32 {
            let sx = x1 + ox;
            let sy = y1 + oy;
            if sx >= 0 && sy >= 0 && (sx as u32) < src.width() && (sy as u32) < src.height() {
                out.put_pixel(ox as u32, oy as u32, *src.get_pixel(sx as u32, sy as u32));
            }
        }
    }
    out
}

/// 多边形裁切（对齐 petpet 的 `ImagePolygonCropper`）：输出多边形包围盒，其余透明
pub fn crop_polygon(src: &RgbaImage, points: &[(f32, f32)]) -> RgbaImage {
    if points.len() < 3 {
        return src.clone();
    }
    let min_x = points.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
    let min_y = points.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
    let max_x = points.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
    let max_y = points.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
    let (bx, by) = (min_x as i32, min_y as i32);
    let w = ((max_x as i32) - bx).max(1) as u32;
    let h = ((max_y as i32) - by).max(1) as u32;
    let mut out = RgbaImage::new(w, h);
    for oy in 0..h as i32 {
        for ox in 0..w as i32 {
            let x = bx + ox;
            let y = by + oy;
            if !point_in_polygon(points, x as f32 + 0.5, y as f32 + 0.5) {
                continue;
            }
            if x >= 0 && y >= 0 && (x as u32) < src.width() && (y as u32) < src.height() {
                out.put_pixel(ox as u32, oy as u32, *src.get_pixel(x as u32, y as u32));
            }
        }
    }
    out
}

/// `Polygon.contains` 的等价实现（射线法，边界按包含处理）
fn point_in_polygon(points: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut inside = false;
    let count = points.len();
    let mut j = count - 1;
    for i in 0..count {
        let (xi, yi) = points[i];
        let (xj, yj) = points[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// `toAbsoluteCoords`：把四个角点平移到左上角为原点，锚点补回同样的偏移
pub fn absolute_coords(points: [[f32; 2]; 4], anchor: (f32, f32)) -> ([[f32; 2]; 4], (f32, f32)) {
    let mut ltx = f32::MAX;
    let mut lty = f32::MAX;
    for point in &points {
        ltx = ltx.min(point[0]);
        lty = lty.min(point[1]);
    }
    let mut out = [[0.0f32; 2]; 4];
    for (index, point) in points.iter().enumerate() {
        out[index] = [point[0] - ltx, point[1] - lty];
    }
    (out, (anchor.0 + ltx, anchor.1 + lty))
}

/// P4A 透视变形（对齐 petpet 的 `ImageDeformer.computeImage`）
pub fn deform(src: &RgbaImage, points: [[f32; 2]; 4]) -> RgbaImage {
    let (w, h) = (src.width() as f32, src.height() as f32);
    if w < 1.0 || h < 1.0 {
        return src.clone();
    }
    let deformed = projection_matrix(&points);
    let origin_to_deformed = mul(deformed, rect_matrix(w, h));
    let mut to_origin = origin_to_deformed;
    to_origin.invert();
    let corner = |x: f32, y: f32| origin_to_deformed.transform(x, y);
    let p0 = corner(0.0, 0.0);
    let p1 = corner(0.0, h);
    let p2 = corner(w, h);
    let p3 = corner(w, 0.0);
    let poly = [
        (p0.0 as i32, p0.1 as i32),
        (p1.0 as i32, p1.1 as i32),
        (p2.0 as i32, p2.1 as i32),
        (p3.0 as i32, p3.1 as i32),
    ];
    let min_x = poly.iter().map(|p| p.0).min().unwrap_or(0);
    let min_y = poly.iter().map(|p| p.1).min().unwrap_or(0);
    let max_x = poly.iter().map(|p| p.0).max().unwrap_or(0);
    let max_y = poly.iter().map(|p| p.1).max().unwrap_or(0);
    let out_w = (max_x - min_x).max(1) as u32;
    let out_h = (max_y - min_y).max(1) as u32;
    let poly_f: Vec<(f32, f32)> = poly.iter().map(|p| (p.0 as f32, p.1 as f32)).collect();
    let mut out = RgbaImage::new(out_w, out_h);
    for y in 0..out_h as i32 {
        for x in 0..out_w as i32 {
            if !point_in_polygon(&poly_f, x as f32, y as f32) {
                continue;
            }
            let (ox, oy) = to_origin.transform(x as f32, y as f32);
            let ox = (ox as i32).min(src.width() as i32 - 1).max(0) as u32;
            let oy = (oy as i32).min(src.height() as i32 - 1).max(0) as u32;
            out.put_pixel(x as u32, y as u32, *src.get_pixel(ox, oy));
        }
    }
    out
}
/// 3x3 投影矩阵（行主序），字段命名与 petpet 的 `Matrix3D` 一致
#[derive(Clone, Copy)]
struct Matrix3D {
    m00: f32,
    m01: f32,
    m02: f32,
    m10: f32,
    m11: f32,
    m12: f32,
    m20: f32,
    m21: f32,
    m22: f32,
}

impl Matrix3D {
    #[allow(clippy::too_many_arguments)]
    fn new(
        m00: f32,
        m01: f32,
        m02: f32,
        m10: f32,
        m11: f32,
        m12: f32,
        m20: f32,
        m21: f32,
        m22: f32,
    ) -> Matrix3D {
        Matrix3D {
            m00,
            m01,
            m02,
            m10,
            m11,
            m12,
            m20,
            m21,
            m22,
        }
    }

    fn determinant(&self) -> f32 {
        self.m00 * (self.m11 * self.m22 - self.m12 * self.m21)
            + self.m01 * (self.m12 * self.m20 - self.m10 * self.m22)
            + self.m02 * (self.m10 * self.m21 - self.m11 * self.m20)
    }

    fn invert(&mut self) {
        let determinant = self.determinant();
        if determinant.abs() < f32::EPSILON {
            return;
        }
        let inv_det = 1.0 / determinant;
        let nm00 = self.m22 * self.m11 - self.m21 * self.m12;
        let nm01 = -(self.m22 * self.m01 - self.m21 * self.m02);
        let nm02 = self.m12 * self.m01 - self.m11 * self.m02;
        let nm10 = -(self.m22 * self.m10 - self.m20 * self.m12);
        let nm11 = self.m22 * self.m00 - self.m20 * self.m02;
        let nm12 = -(self.m12 * self.m00 - self.m10 * self.m02);
        let nm20 = self.m21 * self.m10 - self.m20 * self.m11;
        let nm21 = -(self.m21 * self.m00 - self.m20 * self.m01);
        let nm22 = self.m11 * self.m00 - self.m10 * self.m01;
        self.m00 = nm00 * inv_det;
        self.m01 = nm01 * inv_det;
        self.m02 = nm02 * inv_det;
        self.m10 = nm10 * inv_det;
        self.m11 = nm11 * inv_det;
        self.m12 = nm12 * inv_det;
        self.m20 = nm20 * inv_det;
        self.m21 = nm21 * inv_det;
        self.m22 = nm22 * inv_det;
    }

    fn scale_columns(&mut self, x: f32, y: f32, z: f32) {
        self.m00 *= x;
        self.m01 *= y;
        self.m02 *= z;
        self.m10 *= x;
        self.m11 *= y;
        self.m12 *= z;
        self.m20 *= x;
        self.m21 *= y;
        self.m22 *= z;
    }

    fn transform(&self, x: f32, y: f32) -> (f32, f32) {
        let z = self.m20 * x + self.m21 * y + self.m22;
        if z.abs() < f32::EPSILON {
            return (0.0, 0.0);
        }
        (
            (self.m00 * x + self.m01 * y + self.m02) / z,
            (self.m10 * x + self.m11 * y + self.m12) / z,
        )
    }

    fn transform3(&self, x: f32, y: f32, z: f32) -> (f32, f32, f32) {
        (
            self.m00 * x + self.m01 * y + self.m02 * z,
            self.m10 * x + self.m11 * y + self.m12 * z,
            self.m20 * x + self.m21 * y + self.m22 * z,
        )
    }
}

fn mul(a: Matrix3D, b: Matrix3D) -> Matrix3D {
    Matrix3D::new(
        a.m00 * b.m00 + a.m01 * b.m10 + a.m02 * b.m20,
        a.m00 * b.m01 + a.m01 * b.m11 + a.m02 * b.m21,
        a.m00 * b.m02 + a.m01 * b.m12 + a.m02 * b.m22,
        a.m10 * b.m00 + a.m11 * b.m10 + a.m12 * b.m20,
        a.m10 * b.m01 + a.m11 * b.m11 + a.m12 * b.m21,
        a.m10 * b.m02 + a.m11 * b.m12 + a.m12 * b.m22,
        a.m20 * b.m00 + a.m21 * b.m10 + a.m22 * b.m20,
        a.m20 * b.m01 + a.m21 * b.m11 + a.m22 * b.m21,
        a.m20 * b.m02 + a.m21 * b.m12 + a.m22 * b.m22,
    )
}

fn projection_matrix(p: &[[f32; 2]; 4]) -> Matrix3D {
    let mut m = Matrix3D::new(
        p[0][0], p[1][0], p[2][0], p[0][1], p[1][1], p[2][1], 1.0, 1.0, 1.0,
    );
    let mut inverse = m;
    inverse.invert();
    let (x, y, z) = inverse.transform3(p[3][0], p[3][1], 1.0);
    m.scale_columns(x, y, z);
    m
}

fn rect_matrix(w: f32, h: f32) -> Matrix3D {
    let mut m = Matrix3D::new(0.0, 0.0, w, 0.0, h, h, 1.0, 1.0, 1.0);
    let mut inverse = m;
    inverse.invert();
    let (x, y, z) = inverse.transform3(w, 0.0, 1.0);
    m.scale_columns(x, y, z);
    m
}

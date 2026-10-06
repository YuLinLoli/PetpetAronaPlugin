#![allow(dead_code)] // 模板模型为对齐 petpet 保留了完整字段，部分当前渲染路径尚未用到
//! Arona 功能插件：Petpet 表情包（移植自 Dituon/petpet）。
//!
//! 编译成 `petpet_plugin.dll` 后放进 `<运行目录>/plugins/petpet/`，框架启动时自动装载。
//! 模板素材放 `<运行目录>/data/petpet/templates/<模板名>/`。
//!
//! 模块划分：
//! - [`template`]：模板模型与解析（新版 template.json + 旧版 data.json）
//! - [`render`]：渲染管线（图像/文本元素、多帧 GIF）
//! - [`text`]：文本排版与栅格化
//! - [`assets`]：资源加载与缓存（本地/GIF/URL）
//! - [`imageops`]：缩放、裁切、圆角、旋转与滤镜
//! - [`petpet`]：插件生命周期、命令/事件触发与业务逻辑

mod assets;
mod config;
mod imageops;
mod length;
mod petpet;
mod render;
mod template;
mod text;

pub use petpet::PetpetPlugin;

// 框架装载本 dll 的入口：导出 ABI 号、工具链指纹、宿主上行通道、造实例四个 C 符号。
arona::export_arona_plugin!(PetpetPlugin::new());
//! 插件配置区：`config/petpet/arona.yml`（由框架生成带注释模板并负责热重载）。

use arona::config::arona::PluginConfig;
use serde::{Deserialize, Serialize};

/// 配置区键名，与插件 id 一致
pub const SECTION: &str = "petpet";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PetpetConfig {
    /// 触发指令，默认 `pet`。`pet` 单独出现时回复模板索引。
    pub command: String,
    /// 关键词前缀，默认空（任意首词命中模板 id/别名即触发）。设为 `#` 后需 `#kiss @user`。
    pub command_head: String,
    /// 群聊冷却时间（毫秒），默认 0（不冷却）
    pub cooldown_time: i64,
    /// 同一用户在群内的冷却时间（毫秒），默认 1000
    pub user_cooldown_time: i64,
    /// 冷却中的提示语，留空则不回复。填 `[nudge]` 表示改戳一戳。
    pub in_cooldown_message: String,
    /// 戳一戳总开关（默认 true，全局）；与按群的 `/pet nudge` 开关取 AND
    pub nudge_enabled: bool,
    /// 戳一戳触发随机模板的概率，0 - 1，默认 0.3
    pub nudge_probability: f64,
    /// 全局冷却（毫秒）：所有群 / 用户共用，0 表示不限；对普通触发和戳一戳都生效
    pub global_cooldown_time: i64,
    /// 默认模板 id；留空时自动选择权重最高的模板
    pub default_template: String,
    /// 禁用的模板 id 列表
    pub disabled_templates: Vec<String>,
    /// 头像请求尺寸（会吸附到 40/100/140/640），默认 640
    pub avatar_size: u32,
    /// 模板目录；留空使用 `data/petpet/templates`
    pub templates_dir: String,
    /// 文本字体文件；留空时自动在系统字体目录里寻找中文字体
    pub font_path: String,
    /// At 机器人但没写模板 id 时使用随机模板（默认 true）
    pub random_on_at: bool,
    /// 单条消息生成的动画帧数上限，防止超大 GIF 卡死，默认 120
    pub max_frames: u32,
    /// 单张合成结果的最大边长，超过则等比缩小，默认 1200
    pub max_side: u32,
    /// 资源缓存条数上限，默认 512
    pub cache_entries: usize,
    /// 资源缓存字节上限，默认 256 MiB
    pub cache_bytes: usize,
}

impl Default for PetpetConfig {
    fn default() -> Self {
        PetpetConfig {
            command: "pet".to_string(),
            command_head: String::new(),
            cooldown_time: 0,
            user_cooldown_time: 1000,
            in_cooldown_message: "技能冷却中...".to_string(),
            nudge_enabled: true,
            nudge_probability: 0.3,
            global_cooldown_time: 0,
            default_template: String::new(),
            disabled_templates: Vec::new(),
            avatar_size: 640,
            templates_dir: String::new(),
            font_path: String::new(),
            random_on_at: true,
            max_frames: 120,
            max_side: 1200,
            cache_entries: 512,
            cache_bytes: 256 * 1024 * 1024,
        }
    }
}

impl PluginConfig for PetpetConfig {
    const TITLE: &'static str = "Petpet 表情包";
    const DOC: &'static str = "把模板目录里的模板渲染成图片回复。模板素材放到 data/petpet/templates/<模板名>/ 下，\
支持新版 template.json 与旧版 data.json。改完保存即热生效。";

    fn comment(path: &str) -> Option<&'static str> {
        Some(match path {
            "command" => "触发指令，默认 pet",
            "command_head" => "关键词前缀，默认空；设为 # 后需 #模板名 触发",
            "cooldown_time" => "群聊冷却时间（毫秒），0 表示不冷却",
            "user_cooldown_time" => "同一用户在群内的冷却时间（毫秒）",
            "in_cooldown_message" => "冷却中的提示语；填 [nudge] 表示改戳一戳，留空不回复",
            "nudge_probability" => "戳一戳触发随机模板的概率(0-1)",
            "default_template" => "默认模板 id，留空自动选择",
            "disabled_templates" => "禁用的模板 id 列表",
            "avatar_size" => "头像请求尺寸，会吸附到 40/100/140/640",
            "templates_dir" => "模板目录，留空使用 data/petpet/templates",
            "font_path" => "文本字体文件路径，留空自动寻找系统中文字体",
            "random_on_at" => "At 机器人但未指定模板时是否用随机模板",
            "max_frames" => "动画帧数上限",
            "max_side" => "合成结果最大边长(px)",
            "cache_entries" => "资源缓存条数上限",
            "cache_bytes" => "资源缓存字节上限（默认 256MiB）",
            "nudge_enabled" => "戳一戳总开关（全局）；与按群的 /pet nudge 开关取 AND",
            "global_cooldown_time" => "全局冷却（毫秒）：所有群/用户共用，0 表示不限",
            _ => return None,
        })
    }
}
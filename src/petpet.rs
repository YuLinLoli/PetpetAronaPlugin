//! Petpet 插件：把 petpet 的模板渲染能力接进 Arona 框架。
//!
//! 触发方式与 petpet 一致：
//! - `pet` / `pet <模板>` / `pet <模板> @某人 附加文本`
//! - 或直接发送模板 id / 别名（`kiss @user`、`锤`）
//! - At 某人会替换 `to` 头像，回复/发送图片会替换图像参数

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use arona::admin;
use arona::onebot::EventContext;
use arona::onebot::hooks::{BodyFilter, HookFlow, NoticeKind, event_handler};
use arona::plugin::{AronaPlugin, PluginContext, PluginMeta, PluginRegistrar};
use arona::runtime::chatlog;
use arona::runtime::config::Feature;
use arona::runtime::dispatcher::{CommandContext, CommandRegistration, Permission, handler};
use arona::runtime::log;
use arona::runtime::message::{ForwardMessage, MessageSegment, OutgoingMessage};
use tokio::sync::Semaphore;
use arona::runtime::priority::ListenerPriority;

use crate::assets::{avatar_url, data_uri, AssetStore};
use crate::config::{PetpetConfig, SECTION};
use crate::render::{self, Request};
use crate::template::{self, Template};
use crate::text::FontSet;

/// 分群功能开关的 key（渲染能力）
const FEATURE: &str = "petpet";

/// 分群功能开关的 key（管理指令）；单列一项，关掉渲染后仍能执行 `/pet on`
const FEATURE_ADMIN: &str = "petpet.admin";

/// 分群功能开关的 key（戳一戳）；独立于渲染开关，可单独控制本群是否回应戳一戳
const FEATURE_NUDGE: &str = "petpet.nudge";

/// 插件 id（= `plugins/petpet/`、`config/petpet/arona.yml`、`data/petpet/`）
pub const PLUGIN_ID: &str = "petpet";

/// 模板索引：id → 模板，别名 → 多个 id
#[derive(Default)]
struct Library {
    templates: Vec<Template>,
    by_id: HashMap<String, usize>,
    by_alias: HashMap<String, Vec<usize>>,
    default_index: Option<usize>,
}

impl Library {
    fn get(&self, id_or_alias: &str) -> Option<&Template> {
        if let Some(index) = self.by_id.get(id_or_alias) {
            return self.templates.get(*index);
        }
        let ids = self.by_alias.get(id_or_alias)?;
        let pick = ids.first()?;
        self.templates.get(*pick)
    }

    fn default_template(&self) -> Option<&Template> {
        self.default_index.and_then(|index| self.templates.get(index))
    }

    fn random(&self) -> Option<&Template> {
        let candidates: Vec<&Template> = self
            .templates
            .iter()
            .filter(|template| template.in_random_list)
            .collect();
        if candidates.is_empty() {
            return self.default_template();
        }
        // 用当前时间做选择，避免额外依赖
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(0);
        Some(candidates[now % candidates.len()])
    }

    fn index_text(&self) -> String {
        if self.templates.is_empty() {
            return "暂未加载任何 Petpet 模板，请把模板放进 data/petpet/templates/".to_string();
        }
        self.templates
            .iter()
            .filter(|template| !template.hidden)
            .map(Template::index_line)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// 冷却表（按群号或用户号记账）
/// 全局冷却用的记账 id：真实群号 / QQ 号都大于 0，不会撞上
const GLOBAL_COOLDOWN: i64 = 0;

/// 同时渲染的上限：单次渲染内存峰值不小，并发叠加容易顶爆
const RENDER_CONCURRENCY: usize = 2;

/// 冷却表条目超过这个数量就顺手清一次过期项
const COOLDOWN_PRUNE_AT: usize = 4096;
#[derive(Default)]
struct Cooldown {
    entries: RwLock<HashMap<i64, (Instant, Duration)>>,
}

impl Cooldown {
    fn locked(&self, id: i64) -> bool {
        let guard = match self.entries.read() {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        match guard.get(&id) {
            Some((start, duration)) => start.elapsed() < *duration,
            None => false,
        }
    }

    fn lock(&self, id: i64, millis: i64) {
        if millis <= 0 {
            return;
        }
        if let Ok(mut guard) = self.entries.write() {
            guard.insert(id, (Instant::now(), Duration::from_millis(millis as u64)));
            // 只增不减会随群/用户数一直涨；超阈值时顺手清掉过期项，均摊开销很低
            if guard.len() >= COOLDOWN_PRUNE_AT {
                guard.retain(|_, (start, duration)| start.elapsed() < *duration);
            }
        }
    }

    fn clear(&self) {
        if let Ok(mut guard) = self.entries.write() {
            guard.clear();
        }
    }
}

/// 插件运行态（字段全是 Arc，方便 clone 进各个钩子闭包）
#[derive(Clone)]
struct State {
    assets: Arc<AssetStore>,
    library: Arc<RwLock<Arc<Library>>>,
    font: Arc<RwLock<Option<Arc<FontSet>>>>,
    cooldown: Arc<Cooldown>,
    render_permits: Arc<Semaphore>,
}

impl State {
    fn library(&self) -> Arc<Library> {
        match self.library.read() {
            Ok(guard) => guard.clone(),
            Err(_) => Arc::new(Library::default()),
        }
    }

    fn font(&self) -> Option<Arc<FontSet>> {
        match self.font.read() {
            Ok(guard) => guard.clone(),
            Err(_) => None,
        }
    }
}

pub struct PetpetPlugin {
    state: State,
}

impl Default for PetpetPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl PetpetPlugin {
    pub fn new() -> PetpetPlugin {
        PetpetPlugin {
            state: State {
                assets: Arc::new(AssetStore::with_limits(512, 256 * 1024 * 1024)),
                library: Arc::new(RwLock::new(Arc::new(Library::default()))),
                font: Arc::new(RwLock::new(None)),
                cooldown: Arc::new(Cooldown::default()),
                render_permits: Arc::new(Semaphore::new(RENDER_CONCURRENCY)),
            },
        }
    }

    /// 读取模板目录并重建索引
    fn reload(&self, ctx: &PluginContext) -> Result<usize, String> {
        let config = ctx.config::<PetpetConfig>(SECTION).get();
        self.state
            .assets
            .set_limits(config.cache_entries, config.cache_bytes);
        let dir = templates_dir(ctx, &config);
        if !dir.exists() {
            if let Err(err) = std::fs::create_dir_all(&dir) {
                return Err(format!("创建模板目录 {} 失败: {err}", dir.display()));
            }
            write_readme(&dir);
        }
        let library = load_library(&dir, &config);
        let count = library.templates.len();
        if let Ok(mut guard) = self.state.library.write() {
            *guard = Arc::new(library);
        }
        self.state.cooldown.clear();
        log::info(format!("Petpet：从 {} 载入 {count} 个模板", dir.display()));
        Ok(count)
    }

    /// 旧版本把输入/输出图落盘到 `data/petpet/image/`，现在这两者都走内存，
    /// 启动时顺手删掉遗留目录（只删插件自己的这个子目录，best-effort）。
    fn cleanup_legacy_image_dir(&self, ctx: &PluginContext) {
        let dir = ctx.data_dir().join("image");
        if dir.is_dir() && std::fs::remove_dir_all(&dir).is_ok() {
            log::info(format!("Petpet：已清理旧的落盘临时图目录 {}", dir.display()));
        }
    }

    fn reload_font(&self, ctx: &PluginContext) {
        let config = ctx.config::<PetpetConfig>(SECTION).get();
        let font = FontSet::detect(&config.font_path).map(Arc::new);
        match &font {
            Some(font) => log::info(format!("Petpet：使用字体 {}", font.source)),
            None => log::warning("Petpet：找不到可用字体，文本元素将不会渲染"),
        }
        if let Ok(mut guard) = self.state.font.write() {
            *guard = font;
        }
    }
}

impl AronaPlugin for PetpetPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta::new(
            PLUGIN_ID,
            "Petpet 表情包",
            env!("CARGO_PKG_VERSION"),
            "根据模板与头像生成 Petpet 表情图片（移植自 Dituon/petpet）",
        )
        .with_author("移植：petpet-arona-plugin")
    }

    fn install(&self, registrar: &PluginRegistrar) -> Result<(), String> {
        registrar.feature(Feature {
            key: FEATURE,
            name: "Petpet 表情包",
            description: "根据模板生成 Petpet 表情图片",
        });
        registrar.feature(Feature {
            key: FEATURE_ADMIN,
            name: "Petpet 管理指令",
            description: "`/pet on|off|nudge|ban|unban` 等群开关与黑名单指令（仅群管理员）",
        });
        registrar.feature(Feature {
            key: FEATURE_NUDGE,
            name: "Petpet 戳一戳",
            description: "被戳一戳时随机回复表情（可在本群单独开关）",
        });
        registrar.config::<PetpetConfig>(SECTION);
        Ok(())
    }

    fn configure(&self, ctx: &PluginContext) -> Result<(), String> {
        let state = self.state.clone();

        // 群消息
        let group_ctx = ctx.clone();
        let group_state = state.clone();
        ctx.listen_feature(
            FEATURE,
            &[BodyFilter::GroupMessage],
            ListenerPriority::default(),
            event_handler(move |event: Arc<EventContext>| {
                let state = group_state.clone();
                let ctx = group_ctx.clone();
                async move {
                    handle_message(&state, &ctx, &event, true).await;
                    HookFlow::Pass
                }
            }),
        );

        // 私聊消息
        let private_ctx = ctx.clone();
        let private_state = state.clone();
        ctx.listen_feature(
            FEATURE,
            &[BodyFilter::PrivateMessage],
            ListenerPriority::default(),
            event_handler(move |event: Arc<EventContext>| {
                let state = private_state.clone();
                let ctx = private_ctx.clone();
                async move {
                    handle_message(&state, &ctx, &event, false).await;
                    HookFlow::Pass
                }
            }),
        );

        // 戳一戳（独立开关 FEATURE_NUDGE，关掉渲染也能单独控制）
        let poke_ctx = ctx.clone();
        let poke_state = state.clone();
        ctx.listen_feature(
            FEATURE_NUDGE,
            &[
                BodyFilter::Notice(NoticeKind::Poke),
                BodyFilter::Notice(NoticeKind::Nudge),
            ],
            ListenerPriority::default(),
            event_handler(move |event: Arc<EventContext>| {
                let state = poke_state.clone();
                let ctx = poke_ctx.clone();
                async move {
                    handle_nudge(&state, &ctx, &event).await;
                    HookFlow::Pass
                }
            }),
        );

        // 管理指令：`/pet on|off|...`。命令名 "pet" 会被框架规范化成 "/pet"，
        // 所以不会和渲染用的无前缀触发词打架。
        ctx.commands(vec![CommandRegistration::new(
            vec!["pet".to_string(), "petpet".to_string()],
            "Petpet 群管理：群开关与成员黑名单",
            handler(|context: Arc<CommandContext>, args: Vec<String>| async move {
                let text = admin_command(&context, &args).await;
                let mut message = OutgoingMessage::text(text);
                if context.group_id.is_some()
                    && let Some(id) = context.message_id
                {
                    message = message.with_quote(id);
                }
                Some(message)
            }),
        )
        .with_feature(FEATURE_ADMIN)
        .with_permission(Permission::GroupAdmin)
        .with_usage("/pet on|off|nudge|enable|disable|ban|unban|banlist|status")]);

        Ok(())
    }

    fn start(&self, ctx: &PluginContext) -> Result<(), String> {
        self.cleanup_legacy_image_dir(ctx);
        self.reload_font(ctx);
        let _ = self.reload(ctx)?;
        Ok(())
    }

    fn on_config_reload(&self, ctx: &PluginContext) {
        self.reload_font(ctx);
        if let Err(err) = self.reload(ctx) {
            log::error(format!("Petpet：重载模板失败: {err}"));
        }
    }

    fn stop(&self, _ctx: &PluginContext) {
        self.state.cooldown.clear();
    }
}
// ==================== 消息处理 ====================

/// 触发解析结果
struct Trigger {
    template: Template,
    raw_text: String,
    tokens: Vec<String>,
}

async fn handle_message(state: &State, ctx: &PluginContext, event: &EventContext, is_group: bool) {
    let config = ctx.config::<PetpetConfig>(SECTION).get();
    // 与原版 petpet 的 MessageChain.contentText 对齐：触发文本只取 text 段，
    // 图片 / 艾特 / 引用 / 表情这些段不参与匹配，因此它们谁先谁后都不影响触发。
    let text = content_text(&event.segments);
    if text.is_empty() {
        return;
    }
    let command = config.command.trim();
    let library = state.library();
    if library.templates.is_empty() {
    // 模板库为空是「发了指令没反应」的头号原因：显式回一句提示，别静默
        if !command.is_empty() && text == command {
            reply_text(
                event,
                is_group,
                "Petpet：模板目录为空，请把模板放进 data/petpet/templates/<模板名>/ 后再试",
            )
            .await;
        }
        return;
    }
    // 只发了个触发指令：回复模板索引（不受冷却影响，与 petpet 一致）
    if !command.is_empty() && text == command && !has_target(event) {
        reply_index(ctx, event, &library, is_group).await;
        return;
    }

    let Some(trigger) = parse_trigger(&text, &config, &library, has_target(event), is_group) else {
        return;
    };
    let template = trigger.template.clone();

    // 无斜杠的管理指令（`pet on`）也照常执行；命令表里登记的那份走 `/pet ...`
    if let Some(first) = trigger.raw_text.split_whitespace().next()
        && matches!(
            first,
            "on" | "off"
                | "enable"
                | "disable"
                | "ban"
                | "unban"
                | "banlist"
                | "blacklist"
                | "feature"
                | "nudge"
                | "戳一戳"
                | "status"
                | "plugin"
                | "help"
                | "帮助"
        )
        && text.starts_with(&config.command)
    {
        let allowed = Permission::GroupAdmin.check(
            arona::runtime::config::is_manager(event.user_id()),
            event.sender_role(),
        );
        let reply = if allowed {
            let tokens: Vec<String> = trigger
                .raw_text
                .split_whitespace()
                .map(str::to_string)
                .collect();
            admin_command_raw(event.group_id(), &event.segments, event.quoted(), &tokens).await
        } else {
            Permission::GroupAdmin.deny_message().to_string()
        };
        reply_text(event, is_group, reply).await;
        return;
    }

    let subject = if is_group {
        event.group_id().unwrap_or_default()
    } else {
        event.user_id()
    };
    if state.cooldown.locked(subject)
        || (is_group && state.cooldown.locked(event.user_id()))
        || state.cooldown.locked(GLOBAL_COOLDOWN)
    {
        if !config.in_cooldown_message.is_empty() && config.in_cooldown_message != "[nudge]" {
            reply_text(event, is_group, config.in_cooldown_message.clone()).await;
        }
        return;
    }

    log::debug(format!("Petpet：模板 {} 触发（文本「{text}」）", template.id));
    let list = build_image_refs(ctx, event, &config).await;
    let request = build_request(&template, &list, &trigger, event, ctx, is_group);
    let output = match render_template(state, &template, &request, &config).await {
        Ok(output) => output,
        Err(err) => {
            log::warning(format!("Petpet：渲染模板 {} 失败: {err}", template.id));
            return;
        }
    };

    if let Err(err) = reply_image(event, &output, is_group).await {
        log::warning(format!("Petpet：发送结果失败: {err}"));
        return;
    }

    if is_group {
        state.cooldown.lock(event.user_id(), config.user_cooldown_time);
    }
    state.cooldown.lock(subject, config.cooldown_time);
    state.cooldown.lock(GLOBAL_COOLDOWN, config.global_cooldown_time);
}

async fn handle_nudge(state: &State, ctx: &PluginContext, event: &EventContext) {
    // notice 事件不经过消息链路，黑名单要在这里自行挡一道
    if arona::runtime::config::is_blacklisted(event.user_id(), event.group_id()) {
        return;
    }
    let config = ctx.config::<PetpetConfig>(SECTION).get();
    // 全局总闸（arona.yml 的 nudge_enabled）与按群开关（/pet nudge）取 AND
    if !config.nudge_enabled
        || !arona::runtime::config::feature_enabled(event.group_id(), FEATURE_NUDGE)
    {
        return;
    }
    if state.cooldown.locked(GLOBAL_COOLDOWN) {
        return;
    }
    let probability = config.nudge_probability.clamp(0.0, 1.0);
    if probability <= 0.0 || random01() > probability {
        return;
    }
    let library = state.library();
    if library.templates.is_empty() {
        return;
    }
    let Some(template) = library.random().cloned() else {
        return;
    };
    let subject = event.group_id().unwrap_or_else(|| event.user_id());
    if state.cooldown.locked(subject) {
        return;
    }

    let sender_id = event.user_id();
    let target_id = event
        .field("target_id")
        .and_then(|value| value.as_i64())
        .unwrap_or(sender_id);
    let bot_id = ctx.onebot_config.self_id;
    let sender = ImageRef {
        url: avatar_url(sender_id, config.avatar_size),
        name: event
            .sender_name()
            .map(str::to_string)
            .unwrap_or_else(|| sender_id.to_string()),
    };
    let target = ImageRef {
        url: avatar_url(target_id, config.avatar_size),
        name: target_id.to_string(),
    };
    let bot = ImageRef {
        url: avatar_url(bot_id, config.avatar_size),
        name: ctx.onebot_config.nickname.clone(),
    };
    let list = if target_id == bot_id || target_id == sender_id {
        vec![bot, sender]
    } else {
        vec![sender, target]
    };

    let trigger = Trigger {
        template: template.clone(),
        raw_text: String::new(),
        tokens: Vec::new(),
    };
    let request = build_request(&template, &list, &trigger, event, ctx, true);
    let output = match render_template(state, &template, &request, &config).await {
        Ok(output) => output,
        Err(err) => {
            log::warning(format!("Petpet：戳一戳渲染 {} 失败: {err}", template.id));
            return;
        }
    };
    if let Err(err) = reply_image(event, &output, false).await {
        log::warning(format!("Petpet：戳一戳发送失败: {err}"));
        return;
    }
    state.cooldown.lock(subject, config.cooldown_time.max(3000));
    state.cooldown.lock(GLOBAL_COOLDOWN, config.global_cooldown_time);
}

// ==================== 管理指令 ====================

/// `/pet` 的用法说明
const ADMIN_USAGE: &str = "\
Petpet 管理指令（群管理员可用）：
/pet on | off           开启 / 关闭本群的 Petpet
/pet enable | disable   全局启用 / 停用 Petpet 插件
/pet plugin on | off    在本群整体启用 / 停用 Petpet 插件
/pet feature on | off   只切换本群的渲染功能开关
/pet nudge on | off     单独开关本群的戳一戳响应
/pet ban @某人          把成员加入本群黑名单
/pet unban @某人        把成员移出本群黑名单
/pet banlist            查看本群黑名单
/pet status             查看当前开关状态";

fn switch_value(raw: Option<&String>) -> Option<bool> {
    match raw.map(|value| value.to_ascii_lowercase()).as_deref() {
        Some("on") | Some("开") | Some("开启") | Some("true") | Some("1") => Some(true),
        Some("off") | Some("关") | Some("关闭") | Some("false") | Some("0") => Some(false),
        _ => None,
    }
}

/// 从 @ 段、`@QQ` / 纯数字参数或引用消息里找出目标成员
async fn resolve_target(
    segments: &[MessageSegment],
    quoted: Option<i64>,
    args: &[String],
) -> Option<i64> {
    for segment in segments {
        if let MessageSegment::At(id) = segment
            && *id > 0
        {
            return Some(*id);
        }
    }
    if let Some(id) = args
        .iter()
        .skip(1)
        .find_map(|value| value.trim().trim_start_matches('@').parse::<i64>().ok())
    {
        return Some(id);
    }
    if let Some(quoted) = quoted
        && let Some(record) = chatlog::lookup(quoted).await
        && record.qq > 0
    {
        return Some(record.qq);
    }
    None
}

/// 管理指令的统一实现：`/pet ...`
async fn admin_command(context: &CommandContext, args: &[String]) -> String {
    admin_command_raw(context.group_id, &context.segments, context.quoted, args).await
}

/// 管理指令实现（与 `CommandContext` 解耦，事件钩子里的无斜杠写法也能复用）
async fn admin_command_raw(
    group: Option<i64>,
    segments: &[MessageSegment],
    quoted: Option<i64>,
    args: &[String],
) -> String {
    let sub = args
        .first()
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    match sub.as_str() {
        "" | "help" | "帮助" | "?" => ADMIN_USAGE.to_string(),
        "status" | "状态" => {
            let global = arona::runtime::config::plugin_enabled(PLUGIN_ID);
            let mut text = format!("Petpet 全局：{}", if global { "启用" } else { "停用" });
            match group {
                Some(group_id) => {
                    let in_group = arona::runtime::config::plugin_enabled_in_group(
                        PLUGIN_ID,
                        Some(group_id),
                    );
                    let feature = arona::runtime::config::feature_enabled(Some(group_id), FEATURE);
                    let nudge =
                        arona::runtime::config::feature_enabled(Some(group_id), FEATURE_NUDGE);
                    let setting = arona::runtime::config::group_setting(group_id);
                    text.push_str(&format!(
                        "\n本群插件：{}\n本群 Petpet：{}\n本群戳一戳：{}\n本群黑名单：{} 人",
                        if in_group { "启用" } else { "停用" },
                        if feature { "开启" } else { "关闭" },
                        if nudge { "开启" } else { "关闭" },
                        setting.blacklist.len()
                    ));
                }
                None => text.push_str("\n（私聊里没有群开关）"),
            }
            text
        }
        "on" | "off" | "feature" => {
            let value = if sub == "feature" {
                switch_value(args.get(1))
            } else {
                switch_value(args.first())
            };
            let Some(value) = value else {
                return "用法：/pet on|off 或 /pet feature on|off".to_string();
            };
            let Some(group_id) = group else {
                return "该指令只能在群里使用".to_string();
            };
            match admin::set_group_feature(group_id, FEATURE, value) {
                Ok(()) => format!("本群 Petpet 已{}", if value { "开启" } else { "关闭" }),
                Err(err) => format!("操作失败：{err}"),
            }
        }
        "nudge" | "戳一戳" => {
            let Some(value) = switch_value(args.get(1)) else {
                return "用法：/pet nudge on|off".to_string();
            };
            let Some(group_id) = group else {
                return "该指令只能在群里使用".to_string();
            };
            match admin::set_group_feature(group_id, FEATURE_NUDGE, value) {
                Ok(()) => format!("本群戳一戳已{}", if value { "开启" } else { "关闭" }),
                Err(err) => format!("操作失败：{err}"),
            }
        }
        "plugin" => {
            let Some(value) = switch_value(args.get(1)) else {
                return "用法：/pet plugin on|off".to_string();
            };
            let Some(group_id) = group else {
                return "该指令只能在群里使用".to_string();
            };
            match admin::set_group_plugin_enabled(group_id, PLUGIN_ID, value) {
                Ok(()) => format!(
                    "本群已{} Petpet 插件{}",
                    if value { "启用" } else { "停用" },
                    if value {
                        ""
                    } else {
                        "（管理指令在本群也会一起停用，可在管理面板恢复）"
                    }
                ),
                Err(err) => format!("操作失败：{err}"),
            }
        }
        "enable" | "disable" => {
            let value = sub == "enable";
            match admin::set_plugin_enabled(PLUGIN_ID, value) {
                Ok(note) => format!(
                    "Petpet 插件已全局{}：{note}",
                    if value { "启用" } else { "停用" }
                ),
                Err(err) => format!("操作失败：{err}"),
            }
        }
        "ban" | "unban" => {
            let Some(group_id) = group else {
                return "该指令只能在群里使用".to_string();
            };
            let Some(user_id) = resolve_target(segments, quoted, args).await else {
                return "请 @ 要操作的人、回复他的消息，或直接写 QQ 号".to_string();
            };
            let blacklisted = sub == "ban";
            match admin::set_group_blacklist(group_id, user_id, blacklisted) {
                Ok(()) => format!(
                    "已把 {user_id} {}本群黑名单",
                    if blacklisted { "加入" } else { "移出" }
                ),
                Err(err) => format!("操作失败：{err}"),
            }
        }
        "banlist" | "blacklist" | "黑名单" => {
            let Some(group_id) = group else {
                return "该指令只能在群里使用".to_string();
            };
            let setting = arona::runtime::config::group_setting(group_id);
            if setting.blacklist.is_empty() {
                "本群黑名单为空".to_string()
            } else {
                let list = setting
                    .blacklist
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join("、");
                format!("本群黑名单（{} 人）：{list}", setting.blacklist.len())
            }
        }
        other => format!("未知参数「{other}」\n{ADMIN_USAGE}"),
    }
}

// ==================== 触发解析 ====================

fn parse_trigger(
    text: &str,
    config: &PetpetConfig,
    library: &Library,
    target: bool,
    is_group: bool,
) -> Option<Trigger> {
    let command = config.command.trim();
    let lookup = |name: &str| -> Option<Template> {
        let template = library.get(name)?;
        if config.disabled_templates.iter().any(|id| id == &template.id) {
            return None;
        }
        Some(template.clone())
    };

    if !command.is_empty() && text.starts_with(command) {
        let rest = text[command.len()..].trim();
        if rest.is_empty() {
            // 只发了个 pet：有 @/图就随机，否则回索引
            if target {
                if !config.random_on_at {
                    return None;
                }
                let template = library.random()?.clone();
                return Some(Trigger {
                    template,
                    raw_text: String::new(),
                    tokens: Vec::new(),
                });
            }
            return None;
        }
        let tokens: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
        let first = tokens.first().cloned().unwrap_or_default();
        if let Some(template) = lookup(&first) {
            return Some(Trigger {
                template,
                raw_text: rest[first.len()..].trim().to_string(),
                tokens: tokens.into_iter().skip(1).collect(),
            });
        }
        // 模板 id 不认识：回退默认模板，整段作为文本参数
        let template = library.default_template()?.clone();
        return Some(Trigger {
            template,
            raw_text: rest.to_string(),
            tokens,
        });
    }

    let head = config.command_head.trim();
    if !head.is_empty() {
        if !text.starts_with(head) {
            return None;
        }
        let rest = text[head.len()..].trim();
        let tokens: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
        let first = tokens.first().cloned().unwrap_or_default();
        let template = lookup(&first)?;
        return Some(Trigger {
            template,
            raw_text: rest[first.len()..].trim().to_string(),
            tokens: tokens.into_iter().skip(1).collect(),
        });
    }

    // 无前缀：首词命中模板 id / 别名即触发
    let tokens: Vec<String> = text.split_whitespace().map(str::to_string).collect();
    let first = tokens.first()?;
    let template = lookup(first)?;
    let _ = is_group;
    Some(Trigger {
        template,
        raw_text: text[first.len()..].trim().to_string(),
        tokens: tokens.into_iter().skip(1).collect(),
    })
}

/// 群里的回复都引用触发的那条消息（原版 petpet 的 `reply` 段行为）；私聊不引用。
fn prepend_quote(message: &mut OutgoingMessage, event: &EventContext, is_group: bool) {
    if is_group && let Some(id) = event.message_id() {
        message.segments.insert(0, MessageSegment::Reply(id));
    }
}

/// 引用 + 文本回复
async fn reply_text(event: &EventContext, is_group: bool, text: impl Into<String>) {
    let mut message = OutgoingMessage::text(text);
    prepend_quote(&mut message, event, is_group);
    let _ = event.reply_message(message).await;
}

/// 模板索引：按原版用**合并转发**发出（实现端不支持时框架会自动降级成平铺消息）
async fn reply_index(
    ctx: &PluginContext,
    event: &EventContext,
    library: &Library,
    is_group: bool,
) {
    let index = library.index_text();
    // 群里按原版用合并转发发（实现端不支持时框架会自动降级成平铺消息）；
    // 私聊没有稳定的合并转发通道，直接发文本。
    let message = if is_group {
        let node = ForwardMessage {
            name: "Petpet!".to_string(),
            uin: ctx.onebot_config.self_id,
            content: vec![MessageSegment::Text(index)],
        };
        OutgoingMessage::forward("Petpet 模板列表", vec![node])
    } else {
        OutgoingMessage::text(index)
    };
    let _ = event.reply_message(message).await;
}
/// 触发文本：与原版 petpet 的 MessageChain.contentText 一致，只把 text 段拼起来，
/// 图片 / 艾特 / 引用 / 表情都不进文本（它们在消息里的位置因此不影响触发）。
fn content_text(segments: &[MessageSegment]) -> String {
    let mut out = String::new();
    for segment in segments {
        if let MessageSegment::Text(text) = segment {
            out.push(' ');
            out.push_str(text);
        }
    }
    out.trim_start().to_string()
}

fn has_target(event: &EventContext) -> bool {
    // 引用某条消息也算指定了对象（回复一张图再发 `pet` 是常见用法）
    event.quoted().is_some()
        || event.segments.iter().any(|segment| {
            matches!(
                segment,
                MessageSegment::At(_) | MessageSegment::Image { .. }
            )
        })
}

// ==================== 请求数据构建 ====================

#[derive(Clone, Debug)]
struct ImageRef {
    url: String,
    name: String,
}

async fn build_image_refs(
    ctx: &PluginContext,
    event: &EventContext,
    config: &PetpetConfig,
) -> Vec<ImageRef> {
    let self_id = ctx.onebot_config.self_id;
    let size = config.avatar_size;
    let mut list: Vec<ImageRef> = Vec::new();
    for segment in &event.segments {
        match segment {
            MessageSegment::At(id) if *id != self_id && *id > 0 => list.push(ImageRef {
                url: avatar_url(*id, size),
                name: id.to_string(),
            }),
            MessageSegment::Image {
                url,
                file,
                data,
                file_id,
            } => {
                if let Some(url) = url.as_ref().filter(|url| !url.is_empty()) {
                    list.push(ImageRef {
                        url: url.clone(),
                        name: "图片".to_string(),
                    });
                } else if let Some(bytes) = data {
                    // 内联输入图：不落盘，直接编成 data URI 交给渲染层解码
                    list.push(ImageRef {
                        url: data_uri("image/png", bytes),
                        name: "图片".to_string(),
                    });
                } else if let Some(file) = file.as_ref().filter(|file| !file.is_empty()) {
                    list.push(ImageRef {
                        url: file.clone(),
                        name: "图片".to_string(),
                    });
                } else if file_id.as_ref().is_some_and(|id| !id.is_empty()) {
                    // 只剩 OneBot 的 file_id：先调 get_image 换直链
                    let file = file_id.clone().unwrap_or_default();
                    if let Ok(value) = event
                        .api()
                        .call("get_image", serde_json::json!({ "file": file }))
                        .await
                        && let Some(url) = value
                            .get("url")
                            .and_then(|value| value.as_str())
                            .filter(|url| !url.is_empty())
                    {
                        list.push(ImageRef {
                            url: url.to_string(),
                            name: "图片".to_string(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    // 引用的消息里带图时，把它当作被操作对象
    if let Some(quoted) = event.quoted()
        && let Some(record) = chatlog::lookup(quoted).await
        && !record.image.trim().is_empty()
        && !list.iter().any(|entry| entry.url == record.image)
    {
        list.push(ImageRef {
            url: record.image.clone(),
            name: if record.name.trim().is_empty() {
                "图片".to_string()
            } else {
                record.name.clone()
            },
        });
    }

    let sender = ImageRef {
        url: avatar_url(event.user_id(), size),
        name: event
            .sender_name()
            .map(str::to_string)
            .unwrap_or_else(|| event.user_id().to_string()),
    };
    let bot = ImageRef {
        url: avatar_url(self_id, size),
        name: ctx.onebot_config.nickname.clone(),
    };
    if list.is_empty() {
        list.push(bot);
        list.push(sender);
    } else if list.len() == 1 {
        list.insert(0, sender);
    }
    list
}

fn build_request(
    template: &Template,
    list: &[ImageRef],
    trigger: &Trigger,
    event: &EventContext,
    ctx: &PluginContext,
    is_group: bool,
) -> Request {
    let last = list.len().saturating_sub(1);
    let second_last = last.saturating_sub(1);
    let size = ctx.config::<PetpetConfig>(SECTION).get().avatar_size;
    let mut images = HashMap::new();
    for key in &template.image_keys {
        let value = match key.as_str() {
            "from" => list.get(second_last).map(|entry| entry.url.clone()),
            "to" => list.get(last).map(|entry| entry.url.clone()),
            "group" => event
                .group_id()
                .map(|id| avatar_url(id, size)),
            "bot" => Some(avatar_url(ctx.onebot_config.self_id, size)),
            other => other
                .parse::<usize>()
                .ok()
                .and_then(|index| list.get(index))
                .map(|entry| entry.url.clone()),
        };
        if let Some(value) = value {
            images.insert(key.clone(), value);
        }
    }

    let mut texts = HashMap::new();
    for key in &template.text_keys {
        let value = match key.as_str() {
            "from" => list.get(second_last).map(|entry| entry.name.clone()),
            "to" => list.get(last).map(|entry| entry.name.clone()),
            "group" => event.group_id().map(|id| id.to_string()),
            "bot" => Some(ctx.onebot_config.nickname.clone()),
            "raw" | "text" => Some(trigger.raw_text.clone()),
            other => other
                .parse::<usize>()
                .ok()
                .and_then(|index| trigger.tokens.get(index).cloned()),
        };
        if let Some(value) = value {
            texts.insert(key.clone(), value);
        }
    }
    let _ = is_group;

    Request { images, texts }
}

async fn render_template(
    state: &State,
    template: &Template,
    request: &Request,
    config: &PetpetConfig,
) -> Result<render::Output, String> {
    let _permit = state
        .render_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|err| format!("渲染限流器已关闭: {err}"))?;
    let font = state.font();
    render::render(
        template,
        request,
        &state.assets,
        font.as_deref(),
        config.max_frames,
        config.max_side,
    )
    .await
}

async fn reply_image(
    event: &EventContext,
    output: &render::Output,
    quote: bool,
) -> Result<(), String> {
    // 输出图始终走内存：框架实现端默认就内嵌 base64，是否改直传 file:// 由框架的
    // send_image_as_file 决定（media_value 的取值优先级：URL > 字节 > 本地文件）。
    let mut message = OutgoingMessage::image_data(output.bytes.clone());
    prepend_quote(&mut message, event, quote);
    event
        .reply_message(message)
        .await
        .map_err(|err| format!("{err:?}"))?;
    Ok(())
}

fn random01() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.subsec_nanos() % 10_000) as f64 / 10_000.0)
        .unwrap_or(0.5)
}
// ==================== 模板目录与索引 ====================

fn templates_dir(ctx: &PluginContext, config: &PetpetConfig) -> PathBuf {
    let configured = config.templates_dir.trim();
    if configured.is_empty() {
        return ctx.data_dir().join("templates");
    }
    let path = PathBuf::from(configured);
    if path.is_absolute() {
        path
    } else {
        ctx.data_dir().join(path)
    }
}

fn load_library(dir: &Path, config: &PetpetConfig) -> Library {
    let mut library = Library::default();
    let Ok(reader) = std::fs::read_dir(dir) else {
        return library;
    };
    for entry in reader.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = path.file_name().and_then(|name| name.to_str()).map(str::to_string) else {
            continue;
        };
        match template::load_dir(&id, &path) {
            Ok(template) => {
                if config.disabled_templates.iter().any(|disabled| disabled == &template.id) {
                    continue;
                }
                library.templates.push(template);
            }
            Err(err) => log::warning(format!("Petpet：跳过模板 {id}: {err}")),
        }
    }
    library.templates.sort_by(|a, b| a.id.cmp(&b.id));
    for (index, template) in library.templates.iter().enumerate() {
        library.by_id.insert(template.id.clone(), index);
        for alias in &template.aliases {
            library
                .by_alias
                .entry(alias.clone())
                .or_default()
                .push(index);
        }
    }

    // 默认模板：配置指定 > 权重最高 > 第一个
    if !config.default_template.trim().is_empty() {
        library.default_index = library.by_id.get(config.default_template.trim()).copied();
    }
    if library.default_index.is_none() {
        let weighted = library
            .templates
            .iter()
            .enumerate()
            .filter(|(_, template)| template.default_weight > 0)
            .max_by_key(|(_, template)| template.default_weight)
            .map(|(index, _)| index);
        library.default_index = weighted.or(if library.templates.is_empty() {
            None
        } else {
            Some(0)
        });
    }
    library
}

fn write_readme(dir: &Path) {
    let text = "\
Petpet 模板目录
=============

把 petpet 模板（每个模板一个子目录）放进这里，子目录里要有 template.json 或 data.json。

  <本目录>/kiss/template.json
  <本目录>/kiss/*.png

模板素材可从 https://github.com/Dituon/petpet-templates 获取。
本插件支持新版 template.json 与旧版 data.json；模板里的 main.js 动态脚本暂不支持。

改完模板后在管理面板重载插件，或改一下 config/petpet/arona.yml 触发热重载。
";
    let _ = std::fs::write(dir.join("README.txt"), text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_value_accepts_common_forms() {
        let value = |raw: &str| super::switch_value(Some(&raw.to_string()));
        assert_eq!(value("on"), Some(true));
        assert_eq!(value("OFF"), Some(false));
        assert_eq!(value("开启"), Some(true));
        assert_eq!(value("关闭"), Some(false));
        assert_eq!(value("1"), Some(true));
        assert_eq!(value("0"), Some(false));
        assert_eq!(value("maybe"), None);
    }

    #[test]
    fn nudge_switch_is_its_own_feature_key() {
        // 戳一戳有独立的分群开关，和渲染 key 不重名
        assert_eq!(FEATURE, "petpet");
        assert_eq!(FEATURE_NUDGE, "petpet.nudge");
        assert!(FEATURE_NUDGE.starts_with(FEATURE) && FEATURE_NUDGE != FEATURE);
        // 帮助文本里要能看到这个单独开关
        assert!(ADMIN_USAGE.contains("/pet nudge on | off"));
    }

    #[test]
    fn library_resolves_id_and_alias() {
        let mut library = Library::default();
        library.templates.push(Template {
            id: "kiss".to_string(),
            dir: PathBuf::from("."),
            kind: template::Kind::Image,
            canvas: template::Canvas::default(),
            elements: Vec::new(),
            delays: vec![65],
            aliases: vec!["亲亲".to_string()],
            description: None,
            in_random_list: true,
            default_weight: 0,
            hidden: false,
            preview: None,
            image_keys: Default::default(),
            text_keys: Default::default(),
            avatar_sizes: Default::default(),
        });
        library.by_id.insert("kiss".to_string(), 0);
        library.by_alias.insert("亲亲".to_string(), vec![0]);
        assert_eq!(library.get("kiss").unwrap().id, "kiss");
        assert_eq!(library.get("亲亲").unwrap().id, "kiss");
        assert!(library.get("nope").is_none());
        assert_eq!(library.index_text(), "kiss  ( 亲亲 )");
    }

    fn kiss_library() -> Library {
        let mut library = Library::default();
        library.templates.push(Template {
            id: "kiss".to_string(),
            dir: PathBuf::from("."),
            kind: template::Kind::Image,
            canvas: template::Canvas::default(),
            elements: Vec::new(),
            delays: vec![65],
            aliases: vec!["亲亲".to_string()],
            description: None,
            in_random_list: true,
            default_weight: 0,
            hidden: false,
            preview: None,
            image_keys: Default::default(),
            text_keys: Default::default(),
            avatar_sizes: Default::default(),
        });
        library.by_id.insert("kiss".to_string(), 0);
        library.by_alias.insert("亲亲".to_string(), vec![0]);
        library.default_index = Some(0);
        library
    }

    /// 触发文本只取 text 段：艾特 / 图片 / 引用不参与匹配，谁先谁后都一样
    #[test]
    fn trigger_text_takes_only_text_segments() {
        assert_eq!(
            content_text(&[MessageSegment::At(123), MessageSegment::Text(" 亲亲".into())]),
            "亲亲"
        );
        assert_eq!(
            content_text(&[
                MessageSegment::Image {
                    url: None,
                    file: None,
                    data: None,
                    file_id: None,
                },
                MessageSegment::Text(" pet".into()),
            ]),
            "pet"
        );
        assert_eq!(
            content_text(&[MessageSegment::Text("kiss".into()), MessageSegment::At(1)]),
            "kiss"
        );
        assert_eq!(content_text(&[MessageSegment::At(1)]), "");
    }

    /// 原版语义：`pet` 带目标 → 随机模板；`pet <模板>` → 指定模板；裸 id/别名 → 该模板
    #[test]
    fn parse_trigger_matches_original_forms() {
        let library = kiss_library();
        let config = PetpetConfig::default();
        let hit = |text: &str, target: bool| parse_trigger(text, &config, &library, target, true);
        assert_eq!(hit("pet", true).unwrap().template.id, "kiss");
        assert_eq!(hit("pet kiss", true).unwrap().template.id, "kiss");
        assert_eq!(hit("kiss", false).unwrap().template.id, "kiss");
        assert_eq!(hit("亲亲", true).unwrap().template.id, "kiss");
        assert_eq!(hit("pet 亲亲", false).unwrap().template.id, "kiss");
        let fallback = hit("pet 不存在 附加文本", false).unwrap();
        assert_eq!(fallback.template.id, "kiss");
        assert_eq!(fallback.raw_text, "不存在 附加文本");
        let empty = Library::default();
        assert!(parse_trigger("pet", &config, &empty, true, true).is_none());
    }

    /// 全局冷却用固定键记账：一次写入就能同时挡住所有群 / 用户
    #[test]
    fn global_cooldown_key_is_shared() {
        let cooldown = Cooldown::default();
        assert!(!cooldown.locked(GLOBAL_COOLDOWN));
        cooldown.lock(GLOBAL_COOLDOWN, 60_000);
        assert!(cooldown.locked(GLOBAL_COOLDOWN));
        // 群自己的键与全局键相互独立，处理器同时查两者
        assert!(!cooldown.locked(10001));
    }

    #[test]
    fn zero_cooldown_is_noop() {
        let cooldown = Cooldown::default();
        cooldown.lock(1, 0);
        assert!(!cooldown.locked(1));
    }
}

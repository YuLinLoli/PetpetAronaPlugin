# petpet · Arona 插件

把 [Dituon/petpet](https://github.com/Dituon/petpet) 的模板渲染能力移植进 Arona-rs 框架的
**功能插件**：识别触发词后，按模板把头像与文字合成成表情图片（PNG / GIF 动图）回复出去。

## 发布信息

```text
适用于 Arona-rs 1.1.0（框架契约 1.0.0，符号布局 6）
构建工具链：rustc 1.98.0，x86_64-pc-windows-msvc，--release，crt-static
下载后放进 <框架运行目录>/plugins/petpet/，重启框架
```

产物名是 `petpet_plugin.dll`；插件 id 为 `petpet`，决定 `config/petpet/arona.yml`、
`data/petpet/` 这两个目录。工具链必须与宿主完全一致，对不上会被装载器直接拒载（框架
`PLUGIN_DEVELOPMENT.md` §22）。

## 安装

1. 用同一套工具链编译出 `petpet_plugin.dll`（见「从源码构建」）。
2. 放进 `<运行目录>/plugins/petpet/petpet_plugin.dll`。
3. 重启框架，在管理面板里启用「Petpet 表情包」功能。
4. 首次启动会创建 `data/petpet/templates/` 并写入一份说明文件。
5. 把模板（每个模板一个子目录）放进 `data/petpet/templates/<模板名>/`：
   - 官方素材：<https://github.com/Dituon/petpet-templates>
   - 一个字目录里要有 `template.json`（新版）或 `data.json`（旧版），以及它引用的图片。
     旧版模板的背景就是该目录里的 `0.png`、`1.png`…… 帧序列。
6. 新增/修改模板后，在管理面板重载插件，或改动一次 `config/petpet/arona.yml` 触发热重载。

## 用法

默认触发指令是 `pet`：

| 消息 | 行为 |
| --- | --- |
| `pet` | 回复所有可用模板的索引（该条不受冷却限制） |
| `pet <模板>` | 用指定模板，头像取发送者 + At/图片 |
| `pet <模板> @某人 文本` | 头像按 At 顺序填入，`文本` 作为 `${0}` 等文本参数 |
| `pet @某人` | At 了人但没写模板 → 随机模板（`random_on_at`） |
| `kiss @某人` | `command_head` 为空时，首词命中模板 id / 别名即触发 |
| At 或直接发送图片 | 图片会替换图像参数 |

- **头像映射**：`to` 取最后一个头像（通常是 At 的人），`from` 取倒数第二个，`group` 是群头像，
  `bot` 是机器人头像；没有 At/图片时自动补成 `[bot, 发送者]`。
- **文本映射**：`to` / `from` / `group` / `bot` 是名字，`raw` / `text` 是整段附加文本，
  `0`、`1`…… 取第 n 个空格分隔的文本参数。
- **触发文本只取文字段**（与原版 petpet 的 `MessageChain.contentText` 一致）：图片、@、引用、表情
  都不参与匹配，所以 `@某人 亲亲`、`[图片]pet`、`亲亲[图片]`、`pet @某人` 都能触发，谁先谁后都行。
- **戳一戳**：群内收到戳一戳（Poke / Nudge）时，若 `nudge_enabled` 开启，有 `nudge_probability`
  概率用随机模板生成一张图，并单独计时冷却。
- **回复会引用触发消息**：群内生成的表情图带 `reply` 段引用触发它的那条消息（与原版 petpet 的群回复一致）；私聊不引用。
- **模板索引走合并转发**：群里直接发 `pet` 会用合并转发（节点昵称 `Petpet!`）发出模板列表；私聊或实现端不支持时自动降级成文本 / 平铺消息。
- **GIF 无限循环**：动图模板导出的 GIF 写入 `NETSCAPE2.0` 循环块，和原版一样一直循环播放。
- 冷却：`cooldown_time`（群）、`user_cooldown_time`（用户）与 `global_cooldown_time`
  （全局，所有群 / 用户共用，普通触发与戳一戳都生效）；命中冷却时按 `in_cooldown_message`
  回复，填 `[nudge]` 表示改戳一戳，留空则不回复。

### 管理指令

命令名会按框架设置自动补上前缀 `/`，也就是 `/pet`（别名 `/petpet`），需要群管理员
（或框架管理员）身份：

| 指令 | 说明 |
| --- | --- |
| `/pet on` / `/pet off` | 开启 / 关闭本群的 Petpet（等价 `/pet feature on\|off`） |
| `/pet enable` / `/pet disable` | 全局启用 / 停用 Petpet 插件 |
| `/pet plugin on` / `/pet plugin off` | 在本群整体启用 / 停用 Petpet 插件 |
| `/pet ban @某人` / `/pet unban @某人` | 加入 / 移出本群黑名单（也可回复某人的消息，或直接写 QQ 号） |
| `/pet banlist` | 查看本群黑名单 |
| `/pet status` | 查看当前开关状态 |

不带斜杠的 `pet on` 之类同样会被识别并执行，鉴权走同一套群管理员判断。

### 群开关与黑名单（与 GUI 互通）

- 插件向框架登记了两个可分群开关的功能：`petpet`（渲染）与 `petpet.admin`（管理指令），
  在管理面板「功能开关」页会各自出现一个勾选框，按插件折叠后还能整组开关。
- 群内黑名单直接写进框架的 `group_settings.<群号>.blacklist`，与管理面板「群成员」页的
  开关是**同一份数据**：面板里勾选/取消与 `/pet ban`、`/pet unban` 双向同步。
- 消息事件的黑名单过滤由框架统一完成；戳一戳（notice）事件不走消息链路，插件内单独挡了一道。

## 配置

配置文件由框架生成并热重载：`config/petpet/arona.yml`（区名 `petpet`）。

| 配置项 | 默认值 | 说明 |
| --- | --- | --- |
| `command` | `pet` | 触发指令；单独出现时回复模板索引 |
| `command_head` | 空 | 关键词前缀；设为 `#` 则需 `#kiss @某人` |
| `cooldown_time` | `0` | 群聊冷却（毫秒），0 不冷却 |
| `user_cooldown_time` | `1000` | 同一用户在群内的冷却（毫秒） |
| `in_cooldown_message` | `技能冷却中...` | 冷却提示；`[nudge]` 表示改戳一戳，留空不回复 |
| `nudge_enabled` | `true` | 戳一戳开关；关掉后群里戳机器人不再回应 |
| `nudge_probability` | `0.3` | 戳一戳触发随机模板的概率（0-1） |
| `global_cooldown_time` | `0` | 全局冷却（毫秒），所有群 / 用户共用，0 不冷却 |
| `default_template` | 空 | 默认模板 id，留空自动选权重最高者 |
| `disabled_templates` | `[]` | 禁用的模板 id |
| `avatar_size` | `640` | 头像请求尺寸，会吸附到 40/100/140/640 |
| `templates_dir` | 空 | 模板目录，留空用 `data/petpet/templates` |
| `font_path` | 空 | 字体文件；留空自动在系统字体里找中文字体 |
| `random_on_at` | `true` | At 机器人但没写模板时是否用随机模板 |
| `max_frames` | `120` | 单条消息的动画帧数上限 |
| `max_side` | `1200` | 合成结果最大边长，超出等比缩小 |
| `cache_entries` | `512` | 资源缓存条数上限 |
| `cache_bytes` | `268435456` | 资源缓存字节上限（默认 256 MB） |

## 性能与内存

- **输入 / 输出图都不落盘**：输入图的字节会内联成 `data:` URI，解码后随本次请求一起释放；
  输出图直接交给框架内嵌发送（同机部署可在框架侧开 `send_image_as_file` 改走直传）。
  `data/petpet/image/` 不再写文件，旧版本遗留的会在启动时清理。
- **逐帧流式渲染**：合成时按帧渲染 → 缩放 → 编码后立即释放，不再一次性把整段动画的所有帧
  留在内存里；`max_frames`、`max_side` 同时压制峰值占用。
- **资源缓存双上限**：模板图片 / 解析后的帧数据按「条数 `cache_entries` + 字节 `cache_bytes`」
  双重上限做 LRU 淘汰，超出即从头淘汰最久未用的条目。
- **渲染并发限流**：同时进行的合成任务被限制在固定并发（默认 2），避免多群同时放大图打满内存。
- **冷却表清理**：群 / 用户 / 全局冷却表条目超过阈值时顺手清一次过期项，长跑不涨。

## 模板支持

- **新版 `template.json`**：`type`（`image`/`gif`）、`metadata`（`alias`/`description`/
  `in_random_list`/`default_template_weight`/`hidden`/`preview`）、`canvas`、`delay`/`fps`、
  `elements`（`image` / `avatar` / `text` / `background`）。
- **旧版 `data.json`**：`type`（`IMG`/`GIF`）、`avatar`（`pos` 支持逐帧数组、`fit`、`round`、
  `avatarOnTop`、`crop`、`angle`、`opacity`、`style`/`filter`）、`text`（含 `$txt1[默认]` 旧语法）、
  `background`（画布尺寸/颜色）、`alias`、`delay`、`reverse`、`hidden`。
  `background.size` 支持 `avatar0Width` / `avatar0Height` 这类元素尺寸变量（对称系模板靠它反推画布）。
- **长度表达式**：`px` / `vw` / `vh` / `cw` / `ch` / `%`、四则运算、变量（如 `to_width`），
  以及 `min(a, b)` / `max(a, b)`。
- **图像元素**：
  - `coords` 支持 XYWH 与 **P4A 四点透视**（`[[x,y]×4, [anchorX,anchorY]]`，含 `null` 空帧）；
  - `crop` 支持矩形 `[x1,y1,x2,y2]`（或 `[w,h]`）与多边形；
  - `fit`（`fill`/`cover`/`contain`/`none`）、`position`、圆角、`origin`、`angle`、
    `rotate` 逐帧旋转、`opacity`、`start`/`end` 帧区间、`reverse`。
- **滤镜全集**：`gray`、`binarize`、`mirror`、`flip`、`blur`、`contrast`、`hsb`、`noise`、
  `swirl`、`bulge`、`swim`、`halftone`、`dot_screen`、`denoise`、`oil`、`swing_twist`、
  `mirage`（会按需再取一张「内层图」参与亮度融合）。
- **文本元素**：变量替换 `${name}`、`${name:默认值}`（`:-` / `:=` / `:+` 同样接受）、
  `$name` 短格式、`\` 转义与默认值嵌套；排版支持换行（`break`）、按宽度缩放（`zoom`）、
  对齐（`left`/`center`/`right`）、基线（`top`/`alphabetic`/`middle`/`bottom`）与描边，
  以及 `style`（`BOLD` / `ITALIC` / `BOLD_ITALIC`，由渲染层合成）。

## 已知限制

- 不支持模板里的 `main.js` 动态脚本（petpet 的 script 模块）；只有 `main.js` 的模板会被跳过。
- 模板引用的素材缺失时，该元素会被跳过并在日志里给出提示，而不是整张图渲染失败。
- 文字排版用 fontdue 逐字栅格化，与 Java AWT 的字形度量可能有 1~2px 差异；
  复杂的艺术字效果（阴影、渐变）未移植。

## 从源码构建

```powershell
cd <本仓库>
cargo build --release           # 产物 target/release/petpet_plugin.dll
cargo test                      # 单元测试 + 端到端渲染测试
```

需要与宿主一致的 rustc 1.98.0 与静态 CRT（仓库里的 `rust-toolchain.toml`、
`.cargo/config.toml` 已经钉好）。默认依赖本地框架仓库
`C:/Users/cheng/RustroverProjects/Arona-rs`，换机器时改 `Cargo.toml` 里的 `arona` 路径即可。

用真实模板素材跑一次渲染冒烟（可选）：

```powershell
$env:PETPET_TEMPLATES_DIR = "G:\jvmprojects\petpet-templates\templates\kiss"
$env:PETPET_SAMPLE_IMAGE  = "G:\jvmprojects\petpet\core\src\test\resources\test-images\input.png"
cargo test renders_real_template_dir_smoke -- --ignored --nocapture

# 遍历整个模板目录逐个渲染（需要目录本身）：
$env:PETPET_TEMPLATES_DIR = "G:\jvmprojects\petpet-templates\templates"
cargo test renders_every_template_in_dir -- --ignored --nocapture
```

## 致谢与许可

- 渲染语义、模板格式与头像接口移植自 [Dituon/petpet](https://github.com/Dituon/petpet)（AGPL-3.0）。
- 本插件以 AGPL-3.0-or-later 发布。
# Kumi 在 Live、REAPER 和 Renoise 中能做什么

[English](../en/DAWS.md) · 简体中文 · [日本語](../ja/DAWS.md)

Kumi 能在 Ableton Live、REAPER 和 Renoise 中工作，用的是各个 DAW 自己的说法。本页说明在哪个 DAW 中能做什么。连接方法见[指南](KUMI_GUIDE.md#连接-reaper-或-renoise)，每个修改工具见 [Kumi 如何修改你的工程](KUMI_CHANGES.md)，Live 桥接的细节见[能力矩阵](CAPABILITY_MATRIX.md)。

## 一览

✓ 表示能做；“仅 Live”表示只有 Live 才有；“尚未”说明还缺什么。

| 能力 | Live | REAPER | Renoise |
| --- | --- | --- | --- |
| **修改工程** | | | |
| 带 Kumi 自己撤销的修改，记入 HISTORY | ✓ | ✓ | ✓ |
| 不调用模型就能完成的即时短语 | ✓ | ✓ | ✓ |
| 在 DAW 内运行代码 | ✓ Python | ✓ Lua（ReaScript）、REAPER 的 API 和动作（action） | ✓ Lua |
| 一次停止一切（`/stop`） | ✓ | ✓ | ✓ |
| 知道 DAW 是否正在录音 | ✓ | ✓ | ✓ |
| 在 DAW 的状态栏显示一行 | ✓ | 不能：REAPER 的 API 没有状态栏（改用通知） | ✓ |
| 打开保存的工程 | ✓ | 尚未：设置中的重启不会重新打开它 | 尚未：设置中的重启不会重新打开它 |
| 告诉你 Kumi 关闭期间的变化 | ✓ | ✓ | ✓ |
| 对话跟随保存的工程 | ✓ | ✓ | ✓ |
| **聆听** | | | |
| 聆听轨道或混音 | ✓ | ✓ | ✓ |
| 离线渲染后聆听，再大声播放给你听 | 直接听 Live 播放的声音 | ✓ | ✓ |
| 对照参考曲试听候选，以及在保持打开的装置上运行 `/goal` | ✓ | ✓ | ✓ |
| 判定自己的修改、`/loop`、匹配参考曲 | ✓ | ✓ | ✓ |
| 片段、对象（item）或采样背后的音频文件 | ✓ | ✓ | ✓ |
| 把轨道分离成分轨（stem） | ✓ Live 自己的分离器 | 尚未：没有 Kumi 能用的分离器 | 尚未：没有 Kumi 能用的分离器 |
| 把音频转换为 MIDI | ✓ 鼓、旋律与和声 | 尚未：只有文件中有音高的声部（Basic Pitch） | 尚未：只有文件中有音高的声部（Basic Pitch） |
| **FOCUS** | | | |
| FOCUS 和节拍灯跟随 DAW 的变化 | ✓ | ✓ | ✓ |
| 以树状显示轨道的设备 | ✓ | ✓ FX 链和容器 | ✓ Track DSPs、Doofer 和 Splitter |
| 音符编辑器中的音符 | ✓ | ✓ MIDI 编辑器 | 改为 Pattern Editor 和 Phrase Editor 的行 |
| 时间线一览 | ✓ 编曲视图 | ✓ 编排视图 | 改为 Pattern Matrix |
| 启动器网格 | ✓ 会话视图 | 仅 Live | 仅 Live |
| 指向 DAW 中的对象（**Ask Kumi about this**） | ✓ | 仅 Live | 仅 Live |
| **素材库与教程** | | | |
| 观看视频教程 | ✓ | ✓ | ✓ |
| 查找并载入 DAW 的预设 | ✓ | ✓ | ✓ |
| 列出你自己的工程（`my_sets`） | ✓ | 尚未：不读取 `.rpp` 文件 | 尚未：不读取 `.xrns` 文件 |
| 搜索 DAW 的手册；按 ID 载入浏览器项目 | ✓ | 仅 Live | 仅 Live |
| 要先配置才显示的插件参数 | ✓ | 仅 Live：所有参数本来就在 | 仅 Live：所有参数本来就在 |
| **Live 自己的** | | | |
| Max for Live 设备、Kumi Ears、Kumi 操作 Live 窗口的“手” | ✓ | 仅 Live | 仅 Live |
| Live 的 Extensions SDK、Willington | ✓ | 仅 Live | 仅 Live |

## Live

- Kumi 的桥接是一个 Remote Script，旁边还有 Kumi 的 Live 扩展和 Willington（[能力矩阵](CAPABILITY_MATRIX.md)）。
- 整个计划在 Live 中只算一次撤销（Cmd-Z，Windows 上为 Ctrl-Z）。
- 脚本做不到的事，用 Live 自己的菜单完成：冻结、并轨（bounce）、分轨、音频转 MIDI 等（[Live 自己的命令](KUMI_GUIDE.md#live-自己的命令)）。

## REAPER

通过 Kumi 的扩展（`reaper_kumi`）工作。已在 macOS 上的 REAPER 7.82 验证。

### Kumi 能修改什么

每项修改都是 REAPER 撤销中的一步，整个计划也是一步。全部工具见 [KUMI_CHANGES](KUMI_CHANGES.md#kumi-能在-reaper-中修改什么)。

- **轨道**（`track_add`、`track_move`、`track_delete` 及其他 `track_…`）：添加（也可作为文件夹轨道）、移动、删除、改名、着色、显示或隐藏、展开或折叠文件夹、锁定；轨道编组、轨道模板（`track_template_insert`）、固定轨道层与拼接（`lane_plays`、`lane_name`、`lane_comp`）、剃刀区域（`razor_set`）。
- **混音器**（`track_volume`、`track_pan` 等）：音量、声像及声像模式、宽度、静音、独奏、极性、声道数、送往上级文件夹或主控的发送、FX 链的开关；录音预备、录音输入、监听。
- **路由：** 发送与接收（`send_…`、`receive_add`）、3/4 声道上的侧链、硬件输出（`hw_out_…`）、送往硬件的 MIDI 输出。
- **FX**（`fx_…`）：按 REAPER 的 FX 浏览器中显示的名称添加，删除、移动、复制、旁通、离线；湿声、引脚、预设、插件的完整状态、FX 链文件；容器与并联 FX；参数（一次最多 256 个）、ReaEQ 的频段；参数调制（LFO、音频控制信号、链接、学习）。
- **包络与自动化**（`envelope_…`、`automation_item_…`、`automation_override`）：显示与启用、包络点、自动化对象（automation item）、自动化模式。
- **对象（item）与 take**（`item_…`、`items_glue`、`items_render`、`take_…`、`stretch_markers_…`）：添加、导入文件、移动、分割、修剪、淡入淡出、编组、删除、复制；只在你要求时才粘合（glue）或渲染；take、伸缩标记、take 标记。
- **MIDI**（`notes_…`、`midi_events_…`）：音符的添加、修改、删除、量化、人性化；CC、弯音、音色切换、压力、文本、SysEx。
- **标记、区域与循环**（`marker_…`、`region_add`、`region_render`、`ruler_lane`、`time_selection`、`loop_range`）：标尺层、区域渲染矩阵。
- **速度与工程**（`tempo_set`、`tempo_marker_set`、`metronome_switch`、`project_…`）：速度标记、拍号与渐变、节拍器、网格、时基、备注、标题、作者。
- **鼓组与目标**（`drum_kit`、`scratch_keep`）。
- **脚本**（`run_lua`、`api_call`、`run_action`）。

它还能播放、停止、移动编辑光标，以及保存工程（或 REAPER 的下一个版本）、轨道模板和 FX 链。

### 如何聆听

- 离线渲染这一段（比播放更快），听过之后再大声播放给你听，最长一分钟。单独一条轨道用的是它的推子前分轨。
- REAPER 播放时，它可以边播放边听主输出（默认 8 秒）。渲染会停止 REAPER 的播放。
- 要判定的修改用最后一个 ReaLimit 保持电平，没有时用 Kumi 加上的 JS: Volume Adjustment。目标在轨道的临时副本（“Kumi · try …”）上试候选，最好的留在“Kumi · Goal best”上。

### 工作流程

- 用声音文件制作**鼓组**：每个打击垫一个 ReaSamplOmatic5000，做成打击垫轨道的文件夹或一个容器，踩镲互相切断。
- **来自教程和参考曲的声音：** 先 `watch_video`，再用 ReaEQ、ReaComp、ReaSynth 或你的插件做出来，靠耳朵判定。
- **预设：** 在 REAPER 的资源文件夹中查找并载入 FX 链、轨道模板和工程模板、JSFX 及 FX 预设。

### 限制

- **只有 REAPER 的撤销**能撤回 `run_lua`、`api_call` 和 `run_action`。`run_action` 绝不运行会打开对话框、退出、打开/关闭/保存工程或渲染的动作。
- **只有 Kumi 的撤销**能撤回 REAPER 的撤销跳过的内容：循环播放（repeat）、节拍器开关、ReaEQ 频段的类型或开关、自动化覆盖、工程的备注、标题、作者、网格和时基，以及什么都没开启的参数调制。
- 用 Kumi 的撤销撤回参数调制时，会关掉它的开关，但 REAPER 一旦创建就会保留的设置仍然留着；HISTORY 的那一行会说明。
- 所有轨道一次被全部替换的工程会被当作另一个工程：Kumi 的引用失效，它会重新读取。
- `track_midi_out` 需要在 REAPER 的偏好设置中有 MIDI 输出设备。
- REAPER 被对话框或渲染占用时，修改会立即被拒绝。

## Renoise

通过 Kumi 的工具（`com.kumi.Bridge.xrnx`）工作。已在 macOS 上的 Renoise 3.5.4 验证。

### Kumi 能修改什么

每项修改都是 Renoise 撤销中单独的一步。全部工具见 [KUMI_CHANGES](KUMI_CHANGES.md#kumi-能在-renoise-中修改什么)。

- **歌曲**（`song_bpm`、`song_lpb`、`song_tpl`、`song_groove` 及其他 `song_…`、`metronome_switch`、`edit_settings`）：BPM、LPB、TPL、律动（groove）、节拍器、编辑设置（Follow Player、Edit Step、八度、录音量化）、名称、艺术家、注释、余量（headroom）。
- **轨道**（`track_…`）：音序器轨道、发送轨道和编组轨道的添加、移动、编组、删除、改名、着色、折叠；音符列和效果列；音量、声像、宽度、静音、独奏、输出路由和延迟。
- **模式序列与矩阵**（`slot_…`、`loop_range`、`loop_pattern`）：槽位的插入、删除、移动、克隆、独立（unique）或改播另一个模式；段落（section）、矩阵中的静音和颜色；循环。
- **模式与行**（`notes_write`、`notes_clear`、`notes_transpose`、`notes_humanize`、`lines_copy`、`pattern_…`、`automation_write`）：用 Renoise 的记法写音符、乐器、音量、声像、延迟和效果列；模式的名称、长度和别名（alias）；自动化。
- **Track DSPs**（`device_…`、`doofer_…`、`splitter_set`、`notepad_write`、`meta_destination`）：原生和插件设备、参数、预设、设备链文件（.xrnt）；Doofer 及其宏、Splitter、Notepad、元设备指向哪里。
- **乐器与采样**（`instrument_…`、`sample_…`、`keyzone_…`）：添加、复制、移动、载入（.xrni、.sfz、采样）、用采样文件制作；宏；采样设置、切片、键区（keyzone）、鼓组布局。
- **调制与采样 FX**（`modset_…`、`moddevice_…`、`chain_…`、`chaindevice_…`）。
- **乐句**（`phrase_…`）：添加、映射到琴键、播放方式和行；编写并渲染乐句脚本；.xrnz 文件。
- **插件与 MIDI**（`plugin_…`、`midi_set`）：插件乐器及其参数和音色（program）；MIDI 乐器的输出。
- **目标与脚本**（`scratch_keep`、`run_lua`）。

它还能播放、停止、把一个音符大声播放出来、在 Renoise 中显示某个对象，以及保存歌曲、乐器和设备预设。

### 如何聆听

- 离线渲染这一段，听过之后再大声播放给你听，最长一分钟。单独一条轨道用的是 Renoise 的轨道独奏。
- Renoise 的脚本听不到它正在播放的声音，而且渲染会停止播放：播放时，Kumi 只渲染你指定的那一段。
- 要判定的修改用最后一个 Maximizer 保持电平，没有时用 Kumi 加上的 Gainer。
- Renoise 的演示版不能渲染，所以 Kumi 在那里无法聆听；`kumi doctor` 会说明。

### 工作流程

- **用采样文件制作乐器：** 带静音组的鼓组，或带力度分层的键盘乐器，都放在一个乐器里。
- **乐句：** 用 Renoise 的记法写，或编写乐句脚本再渲染成行。
- 用设备制作 **Doofer**，并给宏命名。
- **来自教程和参考曲的声音：** 先 `watch_video`，再用原生设备或你的插件做出来，靠耳朵判定。

### 限制

- **只有 Renoise 的撤销**能撤回 `run_lua`。
- **只有 Kumi 的撤销**能撤回 Renoise 的撤销跳过的内容：静音、独奏、循环、模式循环、节拍器开关、编辑设置和插件的自动休眠。
- 行的编辑（`notes_write`、`lines_copy`、`phrase_write` 等）经过 Renoise 自己的撤销和重做之后，Kumi 对它们的撤销会被拒绝：它分不清 Renoise 的重做和你自己写下的相同的行。Renoise 的撤销仍然可以撤回。
- 在长时间的会话中，载入和卸下 AU 乐器之后，Renoise 的撤销历史有时会变空（原因不明）。
- FOCUS 的 Mixer 没有电平表：Renoise 的 API 不提供。
- 渲染内容很密的歌曲时，可能会短暂地以忙碌为由被拒绝；请再试一次。

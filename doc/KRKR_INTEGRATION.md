# KRKR Core 接入

`art3m1s-core` 通过独立版本化函数表暴露 Kirikiri/KAG 运行时，不复用
`Art3m1sApiV1`，也不要求宿主直接依赖 native shim 的私有入口。

```text
Host
  -> art3m1s_krkr_get_api_v1
  -> src/ffi/krkr_api.rs
  -> crates/art3m1s-krkr
  -> C++ krkr host shim
```

## 构建 feature

```toml
krkr-engine = ["dep:art3m1s-krkr", "art3m1s-krkr/native-upstream"]
```

启用后，core 动态库导出：

```c
const Art3m1sKrkrApiV1 *art3m1s_krkr_get_api_v1(size_t *out_size);
```

宿主必须同时校验 `struct_size`、`abi_version` 和 `magic`。native shim 自己的
入口名是私有的 `art3m1s_krkr_native_get_api_v1`，core 不会把该符号暴露给宿主。

`runtime_create` 的 game root 可以是 XP3 文件，也可以是游戏目录。目录中存在
`data.xp3` 时优先以其为入口；没有 `data.xp3` 时接受带 `startup.tjs` 的目录或仅含一个
根级 XP3 的目录。

`krkr-engine` 会请求 `native-upstream`。当 `KRKRSDL3_SOURCE_DIR` 和
`KRKRSDL3_BUILD_DIR` 都已配置时，构建真实的 Kirikiri runtime；未配置时暂时回退到
`native-bootstrap` 并输出 Cargo warning。bootstrap 只用于 CI 和 ABI 布局测试，
不能运行游戏。发布构建应显式要求上游 runtime：

```sh
VCPKG_ROOT=/path/to/vcpkg \
KRKRSDL3_SOURCE_DIR=/path/to/krkrsdl3 \
KRKRSDL3_BUILD_DIR=/path/to/krkrsdl3_build \
ART3M1S_KRKR_REQUIRE_UPSTREAM=1 \
cargo build --no-default-features --features krkr-engine
```

发布打包需要把构建后的 `libart3m1s_krkr_host`、其 `Res/` 目录和 C++ runtime
依赖与 core 一起分发。core 会写入 native shim 输出目录的 rpath，同时附加
`@loader_path`（Apple）或 `$ORIGIN`（Linux/Android），允许发布时统一重定位。

## 句柄与生命周期

- `runtime` 是 `uint64_t` 不透明句柄，只能由 `runtime_create` 产生并由
  `runtime_destroy` 释放一次。
- `frame_id` 标识一次 `runtime_acquire_frame`；返回的 `pixels` 指向 core 复制的
  数据，在对应的 `runtime_release_frame` 前有效。
- `runtime_poll_audio_command` 返回的 `payload` 指向 core 暂存区，在下一次 poll
  前有效。宿主需要同步复制 PCM。
- `runtime_submit_audio_consumed` 回传的是当前 stream 自创建或最近一次
  stop/reset 后的绝对已消费 sample frame 数。

core 会复制 native frame 和 audio payload，因此 C++ 借用指针不会直接穿过
公共 ABI。宿主仍不能并发调用同一个 runtime；创建、推进、输入、音频回传和销毁
应固定在同一个 owner 线程。

## 帧循环

```text
rt = runtime_create(game_root, save_root, config)
loop:
    runtime_push_input(rt, events)
    runtime_tick(rt)
    drain_audio(runtime_poll_audio_command, runtime_submit_audio_consumed)
    if runtime_acquire_frame(rt, &frame) == OK:
        consume(frame.pixels, frame.stride, frame.width, frame.height)
        runtime_release_frame(rt, frame.frame_id)
    if runtime_is_exit_requested(rt):
        break
runtime_destroy(rt)
```

`runtime_tick` 当前每次推进一帧；宿主仍需负责真实帧时钟、显示缩放、扬声器播放和
输入坐标转换。

## 已验证范围

2026-09-12 使用 `/Users/alphaly/Downloads/王様恋愛【体験版】/data.xp3` 通过
core ABI 实测：

- 挂载 `data.xp3` 和 `patch.xp3`，启动 KAG 3.32 / Kirikiri 2.32.2。
- 注入标题菜单点击并进入 `TIPlugin_Base.ks -> ADV_Start.ks -> 0_1.ks`。
- 抓取正文首句画面，尺寸 `1920x947`。
- 读取 4 路音频流、374 个 PCM chunk、`8,177,634` 字节非零 PCM。

尚未完成：真实扬声器播放、任意 Windows `.dll`/`.tpm` 插件、视频帧路径、
iOS 动态 framework 打包和发布级 rpath 重定位。

# BAL2D 固件逆向 P0：VGLite / LVGL / 表盘对象

静态分析（IDA 9.4 + Hex-Rays），未上机。2026-11-01。
目标：Band 10 Pro `3.101.043`（下称 **043**）、Band 11 `4.100.179`（下称 **179**）。

> **范围更新（2026-11-02）**：项目只支持 **043**。Band 11 已放弃（屏幕小、空闲堆约 2.7 MB 装不下大多数大厅）。
> 下文的 179 地址保留为反编译参考：表盘解析器（`wf`）两机是同一份代码，很多结论是在 179 的 IDB 上得出、再在 043 上验证的。
> 本文 §5 的若干推断已被上机验证推翻，以文末“§8 上机勘误”为准。

## 0. 方法与证据来源说明

- 179：在 `/Volumes/EXT0/Canopus-Module-BAL2D/.scratch/idb179/` 的 IDB 副本上分析。
  注：首次直接打开 `fwbins/.../179/vela_ap.bin.i64` 后关闭（save=false）时 idalib 仍重打包了该 i64；已用打开期间拷出的原始副本恢复，`cmp` 字节一致、mtime 复原为 Oct 21 13:55。之后只用副本。
- 043：`fwbins/.../043/vela_ap.bin.i64`（及其副本）在本机 idalib 下**打不开**（无错误详情，与并发无关）。改为用 `~/.idapro/loaders/bes_vela.py` 从原始 `vela_ap.bin` 重建 IDB（`.scratch/idb043raw/`）。该库**不含** PSRAM 执行段 0x1C000000 与 SRAM 执行段 0x002Bxxxx 的映射；这两段代码只能在其 flash 加载地址处看到，地址换算见 §1.1。
- 置信度：**确认** = 反编译/反汇编直接可见；**推断** = 由布局/大小/调用关系对照得出，未逐条反编译。

## 1. 结论速览

| 问题 | 结论 |
|---|---|
| 驱动版本 | 两机均为 VGLite 用户驱动 **4.0.47**：`vg_lite_get_info` 返回 api 0x00030000、header 7、release 0x4002F。芯片 GCNanoUltraV（chip 0x265 / rev 0x2002 / cid 0x424，`vg_lite_init` 内校验）。043 与 179 的驱动函数逐个大小几乎相同。 |
| ABI | 固件为 **hard-float（VFP 传参）**。`vg_lite_translate/scale/rotate` 的 float 走 s0/s1；`vg_lite_init_path` 的 bbox 走 s0..s3。target.toml 的 `float_abi="soft"` 若指模块编译方式，调用这些函数会传错参数。规避办法：直接写 matrix/path 结构体，不调用带 float 参数的 API。 |
| 线程 | LVGL 无 OS（LV_USE_OS=NONE）。vg_lite draw unit 在调用 `lv_timer_handler` 的 UI 任务（libuv loop）里**同步**执行。驱动无 mutex。从 lv_timer 回调里自行下发 vg_lite 命令再 `vg_lite_finish` 是安全的，前提见 §3。 |
| GPU 内存 | 专用 vidmem 池 1 MB，经 **0x38 非缓存别名**访问（179：0x38256B40；043：0x38171400），由 `vg_lite_allocate` 分配。LVGL 的 draw buffer 来自普通 umm 堆（0x3C 缓存别名 PSRAM），64 B 对齐，无任何 cache 回调。GPU 直接使用 CPU 指针（address == memory）。 |
| 表盘 Image | Image record 由自定义类 `wfimg`（基类 `lv_image_class`）实例化。UID 存在 `obj+136`，src 是堆中解码出的 `lv_draw_buf_t*`。识别键见 §5。 |
| Lua | luavgl：`src` 为字符串时原样传给 `lv_image_set_src(obj, const char*)`（文件路径 src），为 userdata 时传指针。Lua 对象是普通 lv_obj。 |
| 分辨率 | 179 = 212×520（固件中有 BG 图尺寸判断；Mirage q66 配置相同）。043 = 336×480（**仅** Mirage p67 配置，固件内未验证）。 |

### 1.1 地址约定（模块实际该调用的地址）

- **179**：driver、LVGL、表盘代码都在 XIP flash `0x0C…`，callable = entry | 1。target.toml 的扫描范围 [0x0C000000, +0xD00000) 覆盖全部地址。
- **043**：
  - VGLite 用户驱动（以及部分 LVGL 热点函数）在启动时由 `sub_C0C0AC8` 从 flash [0x2C0FC6CC, 0x2C10D438) 拷到 SRAM [0x200B2860, 0x200C35CC)，经 RAMX 别名 `0x00200000 + (sram − 0x20000000)` 执行（例如 `vg_lite_finish` = RAMX 0x002B3DD0）。flash 里的加载副本地址为 `load = 0x0C0FC6CC + (ramx − 0x2B2860)`，IDA 只能在这个地址看到函数体。**不要调用 load 地址**：代码是为 RAMX 链接的。
  - 固件自身经 8 字节 veneer `LDR.W PC,=ramx|1` 调用。LVGL 用到的 API 的 veneer 在 0x0C187xxx。这段处于 PSRAM 拷贝区 [0x0C10D440, 0x0C18D940)，运行时同时存在于 `rt = 0x1C000000 + (addr − 0x0C10D440)`。flash 副本与 PSRAM 副本内容相同、跳转目标相同，两者都能调用。现有 `p67_blur_gpu_finish` 记录用的是 flash veneer 0x0C187BB1，`lv_image_set_src` 等 PSRAM 区函数体记录用的是 0x1C 运行地址。**建议：veneer 用 0x0C 版本（若有 0x0CAC0xxx 纯 flash veneer 则优先用它）；PSRAM 区里的非 veneer 函数体一律用 0x1C 运行地址。**
  - `vg_lite_allocate`/`vg_lite_free` 在 043 上**没有任何 veneer**，只能直接调 RAMX 地址（0x002B3869 / 0x002B3A75）。该地址不在 043 target.toml 的校验范围内，需要策略决定（放宽范围，或改用 §4 的 lv_draw_buf 方案）。

## 2. VGLite API 地址表（callable = 表中地址 | 1）

原型以 4.0.47 头文件为准，并经 LVGL 调用点核对。`buf`=`vg_lite_buffer_t*`，`mtx`=`vg_lite_matrix_t*`，`path`=`vg_lite_path_t*`。返回值均为 `vg_lite_error_t`（int，0 = 成功）。

| API / 原型 | 179 entry | 043 veneer（0x0C） | 043 运行别名（0x1C） | 043 RAMX 函数体 | 依据 |
|---|---|---|---|---|---|
| `vg_lite_query_feature(u32 f)`（返回 0/1） | 0x0C91E650 | 0x0C187C80 / 0x0CAC0160 | 0x1C07A840 | 0x2B3DB4 | 确认：边界 0x2D，读表 |
| `vg_lite_finish(void)` | 0x0C91E66C | 0x0C187BB0 | 0x1C07A770 | 0x2B3DD0 | 确认 |
| `vg_lite_flush(void)` | 0x0C91F85C | 0x0C1877E8 | 0x1C07A3A8 | 0x2B4FC0 | 确认 |
| `vg_lite_clear(buf, vg_lite_rectangle_t* {x,y,w,h i32}, u32 color ABGR)` | 0x0C91ED0C | 0x0C187AB0 | 0x1C07A670 | 0x2B4470 | 179 确认 / 043 推断 |
| `vg_lite_blit_rect(buf tgt, buf src, rect*, mtx, blend, u32 color, filter)` | 0x0C91EF18 | 0x0C187A58 | 0x1C07A618 | 0x2B467C | 确认（LVGL img 调用点） |
| `vg_lite_draw(tgt, path, fill_rule, mtx, blend, u32 color)` | 0x0C920948 | 0x0C188070 / 0x0CAC0248 | 0x1C07AC30 | 0x2B605C | 179 确认（错误串）/ 043 推断 |
| `vg_lite_draw_pattern(tgt, path, fill, path_mtx, buf img, img_mtx, blend, pattern_mode, u32 pat_color, u32 color, filter)` | 0x0C921010 | 0x0C187F00 | 0x1C07AAC0 | 0x2B6720 | 确认 |
| `vg_lite_set_scissor(i32 x, i32 y, i32 right, i32 bottom)`（right/bottom 不含） | 0x0C9204B8 | 0x0C1876F8 | 0x1C07A2B8 | 0x2B5BCC | 确认 |
| `vg_lite_enable_scissor(void)` | 0x0C9204D4 | 0x0C187A48 | 0x1C07A608 | 0x2B5BE8 | 确认 |
| `vg_lite_disable_scissor` / `vg_lite_scissor_rects` | **不存在** | — | — | — | 未被引用，链接时被裁掉 |
| `vg_lite_set_CLUT(u32 count∈{2,4,16,256}, u32* argb)` | 0x0C9204EC | 0x0C187D20 | 0x1C07A8E0 | 0x2B5C00 | 确认 |
| `vg_lite_set_color_key(vg_lite_color_key4_t*)` | 0x0C92052C | 0x0C187848 | 0x1C07A408 | 0x2B5C40 | 179 确认 / 043 推断 |
| `vg_lite_identity(mtx)` | 0x0C9205D4 | 0x0C187BB8 / 0x0CAC1020 | 0x1C07A778 | 0x2B5CE8 | 确认 |
| `vg_lite_translate(float x, float y, mtx)` [VFP] | 0x0C9205F6 | 0x0C187C18 / 0x0CAC0900 | 0x1C07A7D8 | 0x2B5D0A | 179 确认 |
| `vg_lite_scale(float sx, float sy, mtx)` [VFP] | 0x0C920636 | 0x0C187E00 / 0x0CAC0368 | 0x1C07A9C0 | 0x2B5D4A | 推断（调用点在 scale≠256 分支） |
| `vg_lite_rotate(float deg, mtx)` [VFP] | 0x0C92068C | 0x0C187AF0 | 0x1C07A6B0 | 0x2B5DA0 | 推断（调用点在 angle≠0 分支） |
| `vg_lite_init_path(path, u8 fmt, u8 quality, u32 len, void* data, float minx,miny,maxx,maxy)`：r0..r3 + [sp]=data，bbox 在 s0..s3 | 0x0C92074C | 0x0C187800 | 0x1C07A3C0 | 0x2B5E60 | 两机均确认 |
| `vg_lite_clear_path(path)` | 0x0C920866 | 0x0C1877A0 | 0x1C07A360 | 0x2B5F7A | 179 确认 / 043 推断 |
| `vg_lite_allocate(buf)`（按 w/h/format 算 stride，填 handle/memory/address） | 0x0C91E104 | **无** | — | 0x2B3868 | 两机均确认 |
| `vg_lite_free(buf)` | 0x0C91E310 | **无** | — | 0x2B3A74 | 179 确认 / 043 推断 |
| `vg_lite_get_info(vg_lite_info_t*)` | 0x0C91E3C0 | 0x0C1879E0 | 0x1C07A5A0 | 0x2B3B24 | 确认 |
| `vg_lite_get_product_info(char*, u32* chip, u32* rev)` | 0x0C91E3E4 | 0x0C187F48 | 0x1C07AB08 | 0x2B3B48 | 确认 |
| `vg_lite_get_register(u32 off, u32*)` | 0x0C91E3A0 | 0x0C187CE0 | 0x1C07A8A0 | 0x2B3B04 | 确认 |
| `vg_lite_get_mem_size(u32*)` | 0x0C91E0E8 | 0x0C187698 | 0x1C07A258 | 0x2B384C | 确认 |
| `vg_lite_init(i32 tess_w, i32 tess_h)`（LVGL 已调用，参数 480,480；模块勿调） | 0x0C91E478 | 0x0C588A28 | — | 0x2B3BDC | 确认 |
| `vg_lite_blit`（非 rect 版） | 未找到 | — | — | — | 疑被裁掉；改用 `blit_rect` 传全图 rect |
| `vg_lite_kernel(cmd, void*)`（内部） | 0x0C924D44 | — | — | load 0x0C10424C | 确认。cmd：0 INIT, 1 TERM, 2 ALLOC, 3 FREE, 4 SUBMIT（写 0x500/0x504）, 5 WAIT, 6 RESET, 8 MAP, 9 UNMAP, 10 CHECK, 11 QUERY_MEM, 18 下电 |
| `gpu_memory_setup`（内部） | 0x0C92511C | — | — | 0x0C224AAC（flash） | 确认 |

LVGL 侧辅助函数（模块一般不需要调用）：

| 名称 | 179 | 043（函数体在 PSRAM 区，用运行地址） |
|---|---|---|
| `lv_vg_lite_finish(unit)` | 0x0C3998A8 | 0x0C15C060 → rt 0x1C04EC20 |
| `lv_vg_lite_flush(unit)`（计数 > 7 才调 `vg_lite_flush`） | 0x0C399844 | 0x0C15BFF8 → 0x1C04EBB8 |
| `lv_vg_lite_set_scissor_area(area)` | 0x0C3997B0 | 0x0C15BF5C → 0x1C04EB1C |
| `lv_vg_lite_vg_fmt(cf)` | 0x0C3993CC | 0x0C15B2EC |
| `lv_vg_lite_buffer_from_draw_buf` | 0x0C39C780 | 未单独定位 |
| vg_lite unit `draw_dispatch` / `draw_execute` | 0x0C391354 / 0x0C3903B4 | 未单独定位 |
| `lv_init` | 0x0C3A94C4 | 0x0C1663EC → 0x1C058FAC |
| `lv_draw_buf_create(w,h,cf,stride)`（默认 handlers） | 0x0C8B78BC | 未定位 |
| `lv_draw_buf_create_ex(handlers,w,h,cf,stride)` | 0x0C386C14（已有 Canopus 记录） | 未定位 |

### 2.1 结构体布局（两机相同；179 由 LVGL dump/init 函数确认，043 由 dump/blit 调用点确认）

编译选项为 **short enums**：u8/u16 枚举字段由此而来。

`vg_lite_buffer_t`，sizeof = **176**（LVGL `lv_memzero(buf,176)`）：

| off | 类型 | 字段 |
|---|---|---|
| 0 | i32 | width |
| 4 | i32 | height |
| 8 | i32 | stride（字节） |
| 12 | u8 | tiled（0 linear, 1 tiled） |
| 14 | u16 | format |
| 16 | ptr | handle（`vg_lite_allocate` 填写；外部内存填 0） |
| 20 | ptr | memory（CPU 指针） |
| 24 | u32 | address（GPU 地址；固件中 == memory，平坦映射） |
| 28..79 | | yuv 信息（RGB 格式置 0） |
| 80 | u16 | image_mode：0x1F00 NORMAL，0x1F01 MULTIPLY（LVGL 对 A1..A8 用 multiply） |
| 82 | u8 | transparency_mode（0 = opaque） |
| 172..174 | u8 | fc_enable / scissor_layer / premultiplied（推断。clear 会读 +173；`vg_lite_allocate` 会清 +174） |

`vg_lite_matrix_t`，sizeof = **48**：`float m[3][3]`（行主序：m00@0 m01@4 m02@8 | m10@12 m11@16 m12@20 | m20@24 m21@28 m22@32）+ `scaleX@36, scaleY@40, angle@44`。identity 写入 scaleX=scaleY=1、angle=0。m20/m21 非零即为透视。

`vg_lite_path_t`，sizeof = **72**：

| off | 类型 | 字段 |
|---|---|---|
| 0 | float[4] | bounding_box |
| 16 | u8 | quality |
| 17 | u8 | format（0 S8, 1 S16, 2 S32, 3 FP32） |
| 20..35 | | uploaded（handle@20 …） |
| 40 | u32 | path_length（字节） |
| 44 | ptr | path data |
| 48 | u8 | path_changed |
| 49 | u8 | pdata_internal |
| 50 | u8 | path_type（init 写 2） |
| 52 / 56 / 60 | | stroke / stroke_path / stroke_size（clear_path 会释放） |
| 68 | u8 | add_end |

LVGL 的 `lv_vg_lite_path_t` 为 132 B：base(72) + matrix@72 + … + format_len@128 + has_transform@129。

### 2.2 枚举取值（编译进固件的值）

- **buffer format**：
  - 来源：LVGL dump 名称表（179 `off_2CA1354C`，043 `off_2CCE10D0`）与 `lv_vg_lite_vg_fmt` switch 交叉确认。
  - 取值：0 RGBA8888, **1 BGRA8888**, 2 RGBX8888, 3 BGRX8888, 4 RGB565, **5 BGR565**, 6 RGBA4444, **7 BGRA4444**, 8 BGRA5551, 9 A4, **10 A8**, 11 L8, 12 YUYV, 13 YUY2, 14 ANV12, 15 AYUY2, 16 NV12, 17 YV12, 18 YV24, 19 YV16, 20 NV16, 21–24 *_TILED, 100 INDEX_1, 101 INDEX_2, 102 INDEX_4, **103 INDEX_8**, 104 RGBA2222, 105 BGRA2222, 106 ABGR2222, 107 ARGB2222, 108 ABGR4444, **109 ARGB4444**, 110 ABGR8888, 111 ARGB8888, 112 ABGR1555, 113 RGBA5551, 114 ARGB1555, 115 XBGR8888, 116 XRGB8888, 117 RGBA8888_ETC2_EAC, 118 RGB888, 119 BGR888, 120 ABGR8565, 121 BGRA5658, 122 ARGB8565, 123 RGBA5658。0x1000 以上为 OpenVG 兼容格式。
  - LVGL cf → vg：ARGB8888→1，XRGB8888→3，**RGB565→5**，ARGB8565→121，RGB888→119，L8→11，I1..I8→100..103，A4→9，A8→10，cf 21→8，cf 22→7，cf 23→105，NV12→16，YUY2→13，0x30→117。LVGL 的 "RGB565"（蓝在低位）对应 VG **BGR565=5**。
- **blend**：0 NONE, 1 SRC_OVER, 2 DST_OVER, 3 SRC_IN, 4 DST_IN, 5 MULTIPLY, 6 SCREEN, 7 DARKEN, 8 LIGHTEN, 9 ADDITIVE, 10 SUBTRACT, 11 NORMAL_LVGL, 12 ADDITIVE_LVGL, 13 SUBTRACT_LVGL, 14 MULTIPLY_LVGL, 15 PREMULTIPLY_SRC_OVER。
  - LVGL 映射：源已预乘时 normal→1、add→9、sub→10、mul→5；否则特性 LVGL_SUPPORT 为 0 走同一套，HW_PREMULTIPLY 为 0，因此 normal 仍是 1。表 `word_2CA13D06` = {12,13,11,14} 仅在 LVGL_SUPPORT=1 时使用。
- **filter**：0 POINT, 0x1000 LINEAR, 0x2000 BI_LINEAR（LVGL 有变换时用），0x3000 GAUSSIAN。
- **fill_rule**：0x1900 EVEN_ODD（LVGL 画图片矩形时用），0x1901 NON_ZERO。
- **pattern_mode**：0x1D00 COLOR（LVGL 用），0x1D01 PAD, 0x1D02 REPEAT, 0x1D03 REFLECT。
- **quality**：`VG_LITE_HIGH`。具体值未取出，模块可以不调用 init_path 而直接写字段，见 §2.1。

### 2.3 硬件特性（`s_ftable`；179 @0x200A27BC、043 @0x200C8B40，均来自 .data 初值，运行期只读，两机逐字节相同）

名称取自固件内 LVGL 的特性名表（179 `off_2CA13864`）。

- **有（=1）**：0 IM_INDEX_FORMAT，1 **SCISSOR**，2 BORDER_CULLING，3 RGBA2_FORMAT，7 GLOBAL_ALPHA，8 RGBA8_ETC2_EAC，9 COLOR_KEY，13 **24BIT**，14 DITHER，17 IM_INPUT，25 **SRC_PREMULTIPLIED**，32 NEW_IMAGE_INDEX，33 PARALLEL_PATHS，34 STRIPE_MODE，37 RECTANGLE_TILED_OUT，40 YUY2_INPUT，41 YUV_INPUT，42 YUV_TILED_INPUT，43 AYUV_INPUT，44 **16PIXELS_ALIGN**。
- **无（=0）**：4 QUALITY_8X，5 IM_FASTCLEAR，6 RADIAL_GRADIENT，10 DOUBLE_IMAGE，11 YUV_OUTPUT，12 FLEXA，15 USE_DST，16 PE_CLEAR，18 DEC_COMPRESS，19 LINEAR_GRADIENT_EXT，20 **MASK**，21 MIRROR，22 GAMMA，23 NEW_BLEND_MODE，24 STENCIL，26 **HW_PREMULTIPLY**，27 COLOR_TRANSFORMATION，28 **LVGL_SUPPORT**，29 INDEX_ENDIAN，30 24BIT_PLANAR，31 PIXEL_MATRIX，35 IM_DEC_INPUT，36 GAUSSIAN_BLUR，38 TESSELLATION_TILED_OUT，39 IM_REPEAT_REFLECT，45 DEC_COMPRESS_2_0。
- **透视 / 2.5D**：没有单独的特性位。image 坐标生成器带 w 分量（见 qemu 文档 0x0A18–0x0A22），驱动按完整 3×3 矩阵编程，因此透视 blit 可用。**双线性**：filter 0x2000 可用。
- **含义**：
  - SRC_PREMULTIPLIED=1 且 HW_PREMULTIPLY=0：纹理应按**预乘 alpha**提供；GPU 输出为预乘（LVGL 会给 GPU 绘制的 layer 置 PREMULTIPLIED 标志）。
  - 16PIXELS_ALIGN=1：LVGL 的 stride 规则为宽度向上对齐到 16 px（`width_to_stride` 179 = 0x0C38BE8E），模块 buffer 也按此对齐。

## 3. 线程 / 所有权（Q2）

- **无 draw 线程**：
  - 179 `lv_init` 中 SW draw unit 只有 40 B（无 thread/sync 成员）。
  - 两机都没有 `lv_os_nuttx` / `lv_thread` / `swdraw` 等串。
  - vg_lite unit（292 B，名称 "VG_LITE"）的 dispatch（179 0x0C391354）调用 `draw_execute`，在调用者上下文里同步执行。
- **驱动方式**：LVGL 由 `lv_nuttx_uv_init`（179 0x0C7645DC）挂在 UI 任务的 libuv loop 上（fb poll / input poll / timer resume）。因此 `lv_timer` 回调与 LVGL 绘制在同一线程，回调执行期间 LVGL 不会处于绘制中途。
- **finish/flush 的时机**：
  - 每个 task 结束时 `lv_vg_lite_flush`（累计 8 次才真正调用 `vg_lite_flush`，即异步提交）。
  - 无可调度 task 时，dispatch 调用 `lv_vg_lite_finish`（`vg_lite_finish` + 释放 pending 的图片/渐变/路径）。
  - layer 合成前也会 finish。所以一帧结束后 GPU 空闲，命令缓冲为空。
- **驱动无锁**：全局 `s_context`（179 0x200F4380 / 043 0x200F917C），双命令缓冲乒乓（每个 64 KB，从 vidmem 池分配）。每个 API 入口先调用 `vg_lite_power_check`（179 0x0C9253E0），GPU 若已空闲下电则重新上电初始化。SUBMIT 前取 PM 锁（`pm_change_gpu`，179 0x0C934800）。WAIT 为信号量限时等待，超时后复位 GPU。
- **结论**：在 lv_timer 回调里对**自己的** render target 下发命令，并在返回前调用 `vg_lite_finish`，是安全的。若 LVGL 有残留命令，会被一并提交，无害。不要调用 `lv_vg_lite_finish`（它需要 unit 指针）。
- **模块需要处理的全局状态**：
  1. **scissor**：LVGL 每个 task 都设置 scissor 并保持 enable（固件中没有 disable 函数）。模块绘制前**必须** `vg_lite_set_scissor(0,0,w,h)`（坐标相对自己的 target），否则会被 LVGL 上一个 clip 区（屏幕坐标）裁掉。用完不需要恢复，LVGL 下个 task 会重设。`vg_lite_clear` 同样受 scissor 约束。
  2. **CLUT / color key**：LVGL 每次画 index 图之前都会重新 `set_CLUT`，color key 用完会复位。模块用 INDEX_8 时自行设置即可，不必恢复。
  3. **render target 缓存**：驱动内部保存上一次 target 的 176 B 副本（179 `dword_200F4AB4`）。模块释放自己的 buffer 前必须先 `vg_lite_finish`；用 `vg_lite_free` 释放时驱动会比对这份副本并处理。
  4. 不要调用 dither / global alpha / color transform / masklayer 等会留下全局状态的 API。
  5. tessellation 缓冲为全局，由 `vg_lite_init(480,480)` 分配，路径超过窗口时驱动会自动分块。

## 4. GPU 可访问内存（Q3）

- **PSRAM 别名**：0x3C000000 = cached，0x38000000 = non-cached，0x1C000000 = 执行别名。
  - 依据：Mirage `best1503.c` 的 PSRAM 定义；IDA loader 对 flash 采用 0x2C cached / 0x28 NC 的同构约定；startup 拷贝表。
  - 179 启动表 @0x0C0C02A4：flash 0x2C0D01E0..0x2C326D20 → 0x3C000000..0x3C256B40（数据/代码），随后清零 NC [0x38256B40, 0x38356B40)。
  - 043 启动表 @0x0C0C0B20：PSRAM 代码 0x2C10D440..0x2C18D940 → 0x3C000000（执行 0x1C000000）；NC 区 [0x38171400, 0x38271400)。
  - 0x38 为非缓存属于推断（未找到 MPU 配置代码），中等置信。

| | 179 | 043 |
|---|---|---|
| GPU 寄存器 | 0x40220000 | 0x40220000 |
| vidmem 池（`vg_lite_allocate`） | 0x38256B40, 1 MB（= 0x3C256B40 的 NC 别名） | 0x38171400, 1 MB |
| 池内已被占用 | 2×64 KB 命令缓冲 + tess 缓冲（≈60 KB）+ LVGL 渐变图 | 同左 |
| umm 用户堆（malloc/lv_malloc） | 0x3C356B40, 13 210 304 B（≈12.6 MB）至 0x3CFEFE00 | 0x3C271400 – 0x3DFC0000（≈29.3 MB） |
| PSRAM 总量 | 16 MB（推断） | 32 MB（Mirage） |

- **分配器**：
  - `vg_lite_allocate`：内核 vidmem，首次适配。大小与起点都向上取整到 64 B。返回 NC 地址，handle/memory/address 三者相同。该池是全局共享的 1 MB，余量需在运行时用 `vg_lite_get_mem_size` 查询。
  - LVGL draw buffer：`lv_malloc(size+63)`，align 64，来自 umm 堆（0x3C cached）。默认 handlers（179 0x200BD15C）的 invalidate/flush 回调为 **NULL**。179 的 `lv_malloc` = 0x0C3ABD88，底层 malloc = 0x0C3514EC（即 Canopus 记录的 `bt_alloc`，实为通用堆 malloc）。
  - 对齐要求：buffer 起始地址 64 B；stride = 16 px 对齐后再按格式对齐（LVGL `width_to_stride`）。
- **cache 维护**：

| | 179 | 043 |
|---|---|---|
| 整 D-cache clean（`hal_cache_sync(1)`，写 ctrl 0x07FFC000+0x34） | 0x0C92E6D4（Canopus `background_dcache_sync`） | 无直接记录 |
| 区间 invalidate（`hal_cache_invalidate(1,addr,len)`） | 0x0C7211E8 → RAMX 0x275815 | 0x0C588448 → 0x29D36C（`p67_blur_cache_invalidate`） |
| 区间 clean（`hal_cache_clean`） | 未定位 | 0x0C588F88 → 0x29D338（`p67_blur_cache_clean`；≤0x1FE0 逐行，否则整 cache） |

  - 179 区间函数 0x0C7211E8：驱动对 ≤0x8000 B 的命令缓冲使用它，更大时改用整 cache clean。Canopus 将其命名为 invalidate，但其 clean 语义无法从 ROM 外确认。
  - 使用规则：CPU 写入 cached 堆中的纹理后，在 GPU 读取前做 **clean**（179 用整 cache clean 0x0C92E6D4(1)；043 用区间 clean）。CPU 读取 GPU 输出前做 **invalidate**，并先 clean 以免丢失同 cache line 上的脏数据。若纹理和 target 都放在 `vg_lite_allocate` 的 NC 池里，则不需要 cache 操作（043 上 allocate 有地址限制，见 §1.1）。
- **驱动 OS 堆**：`gpu_memory_setup` 另建一个 16 KB 小堆，供驱动内部 malloc，模块不需要关心。

## 5. 原生表盘 → LVGL（Q4）

调用链（179）：
- `refresh_cur_watchface(force)` 0x0C5F0778
  - 依次调用 `watchface_destroy` 0x0C6BC898 和 `watchface_create(root, id_str)` 0x0C6C3198。
  - `watchface_create` 进入 `render_watchface` 0x0C6C2ED0，后者先建 4 个全屏容器（`lv_obj_create` + LV_PCT(100)，0x0C6BA454），再调用 `renderer_render_watchface` 0x0C6C26F0。
  - `renderer_render_watchface` 按 Layout 列表顺序（每项 20 B：uid@+8、x@+12、y@+14）逐个创建子对象，挂在 **layout 容器**下。按资源类型分发：2 Image → `render_image_create` 0x0C6C03B8；3 ImageArray → 0x0C6BC368；6 Slot（自定义类 0x2CA70AA4）；4/5/7/8 为其它类型。

`render_image_create(parent, resmgr, theme, uid|color_bits)`：
```
rec = resource_manager_get_resource(resmgr, theme, uid);  // 179 0x0C6BFE14：带引用计数的缓存
                                                           // 20 B: {u32 uid, u32 off, u8 cf, u8 flags,
                                                           //        u16 w@12, u16 h@14, lv_draw_buf_t* @16}
obj = lv_obj_class_create_obj(&wfimg_class, parent); lv_obj_class_init_obj(obj);
*(u16*)(obj+128)=0x100; *(u32*)(obj+132)=theme; *(u32*)(obj+136)=uid; *(u32*)(obj+140)=resmgr; *(rec**)(obj+148)=rec;
lv_image_set_src(obj, rec->draw_buf);   // 堆上解码出的 lv_draw_buf_t*（header magic 0x19）
```
- 资源数据先由 `binary_parse_get_image` 0x0C6BD7C8（经 parser vtable 调用）从 BIN 中读出（`wf_heap_get_image_resource` 0x0C88D9BC：按 uid 查表，然后读文件或从内存 blob 取），再**解码成 RAM 中的 lv_draw_buf**。因此 src 指向的不是 BIN 原始字节。
- `wfimg_class`：179 **0x2CA7079C** / 043 **0x2CDBDC40**。基类为 `lv_image_class`（179 0x2CA12E7C，name "image"），类名 "wfimg"，instance_size 152，ctor/dtor/event = 0x0C6B90DB / 0x0C6B9835 / 0x0C6BA4A9（179）。
- 043 对应地址：`render_image_create` 0x0CA89200（字段偏移与 179 相同，确认）；`renderer_render_watchface` 0x0CA877DC；`refresh_cur_watchface` 0x0C5F32B8；create 0x0CA9669C；destroy 0x0CA95F80。

**最可靠的识别键**（模块遍历时逐一检查）：
1. `obj->class_p`（lv_obj_t +0）== `wfimg_class`；
2. `(*(u32*)(obj+136)) & ~0x00780000 == marker_uid`（bits 19..22 为配色索引，renderer 会 OR 进去；高字节类型为 0x02）；
3. 可选校验：`rec = *(void**)(obj+148)`，比对 `rec->w/h`（+12/+14）与 marker 尺寸，再检查 `draw_buf->data` 的像素魔数。

UID 是 BIN 内资源 ID，与渲染无关，不受解码、缓存或主题影响，是首选键。图层序号可作为辅助：marker 是 layout 容器中按 Layout 顺序的第 k 个**成功创建**的子对象，创建失败的项会被跳过。

**定位根对象**：
- 179 全局：`cur_info` 0x200C5F30，`cur_handle` 0x200C5F34，`root`（传给 create 的父对象）0x200C5F38。
- 043 全局：0x20119770 / 0x20119774 / 0x20119778（字段布局按 179 推断）。
- `handle`（84 B）：+0 name[64]，[16] info，[17] root，[18] resmgr，[19] renderer。
- `renderer`（128 B）：[0] root，[17] bg 容器，[18] bg 对象，**[19] layout 容器**，[20] 额外容器，[21] fg 容器，[22] fg 对象，[24] resmgr，[30] 配色索引。
- 也可以不依赖全局：`lv_obj_tree_walk` 从当前屏幕开始，按 class + uid 匹配。

**生命周期**：
- `watchface_create` / `watchface_destroy` 如上。
- `watchface_pause_update_data` 179 0x0C6BC4E0 / 043 0x0CA95A88；`watchface_resume_update_data` 179 0x0C6BC514 / 043 0x0CA95ABC（由 179 0x0C5E8768 等页面可见性逻辑调用）。
- `app_watchface_take_snapshot` 179 0x0C5F12EC（已有记录）。
- **亮屏时不重建**：`refresh_cur_watchface` 在表盘未变化且 force=0 时打印 "ignore. force: 0" 后返回，只有切换表盘、主题/长续航或强制刷新时才 destroy+create。模块仍应把对象指针视为易失：对象可能被销毁，应在 resume 时或周期性地重新查找。
- **替换 src 的注意事项**：
  - wfimg 析构只释放自己的 `rec`（引用计数），不会动模块的 buffer。
  - LVGL 9 对指针型 src 有解码/缓存条目（"VG_LITE" 解码器会做转换，未验证是否拷贝）。每帧更新内容后应 `lv_image_cache_drop(src)` 并 `lv_obj_invalidate`。179 已有 `lv_image_cache_drop` 记录。
  - 替代方案：隐藏 marker，在同一父对象、同一索引处插入模块自己的对象（`lv_obj_move_to_index`）。

## 6. Lua 表盘（Q5，仅 179）

- 绑定为 luavgl：
  - `luavgl_obj_createmetatable` 0x0C8E87AC（`__name`/`__magic="luavglObj"`）；
  - `luavgl_to_obj` 0x0C6C7174（userdata 首字段即 `lv_obj_t*`）；
  - image 属性表 @0x2CA71584 中 `{"src", type 4 IMGSRC, lv_image_set_src}`；方法 `set_src` 0x0C6C84C4。
- `luavgl_toimgsrc` 0x0C6C8490：userdata（light 或 full）→ 指针；字符串 → `lua_tostring` 原样 → `lv_image_set_src(obj, "path")`，即 LVGL 文件型 src（LVGL 内部 strdup，经 lv_fs + BIN/PNG/JPEG 解码器加载）。固件**不做**路径改写。
- Lua 创建的对象是普通 LVGL 子对象，C 侧可用 `lv_obj_tree_walk` 找到。识别方法：取 `lv_image` 的 src，判定为 FILE 类型后比对路径串（需定位 `lv_image_get_src`，或直接读 `lv_image_t.src` 偏移，本次未取）。
- Lua 表盘的根对象位置未确定。043 未分析。

## 7. 其它（Q6）

- **分辨率**：179 = 212×520（`binary_parse_get_image` 对 w==212、h==520 打印 "BG image detected"；Mirage q66 = 212×520）。043 = 336×480（仅 Mirage p67 配置，固件内未核对）。
- **刷新**：
  - `lv_display_create`（179 0x0C384E60）调用 `lv_timer_create(lv_display_refr_timer 0x0C3894D0, 16, disp)`，LV_DEF_REFR_PERIOD = **16 ms**。
  - 实际由 `lv_nuttx_uv` 的 fb poll（vsync/TE）驱动，显示走 NuttX `/dev/fb0`。Mirage 注释称 TE 由 GPIO 中断节拍驱动。flush 是否为异步 DMA：**未确定**。
- **CPU 主频**：**未确定**。Mirage 模型取 SDK 最高档 208 MHz，未在固件中验证。

## 8. 未完成 / 待验证

- 0x38 为 NC 别名：仍是推断（未找到 MPU 配置代码），需上机测试（CPU 写 0x3C 后 GPU 是否可见）。
- 179 区间 cache 函数 0x0C7211E8 的确切语义（clean 还是 invalidate）。
- 043：clear/draw/set_color_key/scale/rotate/clear_path/free 按布局对照推断，未逐个反编译；表盘 handle 与 renderer 字段沿用 179 的布局。
- `VG_LITE_HIGH` 数值、`lv_image_t.src` 偏移、Lua 根对象、CPU 主频、flush 机制。
- 043 原始 IDB 无法打开的原因未查明（建议在 IDA GUI 中打开看报错）。


## 8. 上机勘误（Mirage 043，2026-11-02）

以下结论经 Mirage Band 10 Pro 043 上机验证（固件 syslog：nand_data `/offlinelog/tmp.log`，grep `[wf]`），覆盖 §5 中的对应推断：

1. **043 的表盘根全局推断错误**：0x20119770/74/78 不是表盘对象树的根。模块改为从默认 display 的当前屏幕遍历：
   `disp_def` 179 0x200BD0A4 / 043 **0x2010318C**（取自 `lv_display_get_layer_top` 的字面量 0x20103174 + 0x18），
   `lv_display_t`：`act_scr` +0x2B8、`top_layer` +0x2C8（LVGL 9.0 布局，两机一致）。当前屏幕下约 450 个对象，表盘 wfimg 在深度 11–13。
   本固件 `spec_attr`：children 在 +0，`child_cnt`（u16）在 +48。
2. **布局数据必须连续**：`binary_parse_get_layout_list`（179 0x0C6C3784）只读取第一条布局记录的地址，然后以 16 B 为步长**一次性**读取 `16 × count` 字节，
   按记录顺序配对。追加在别处的布局数据会被忽略，并使后面所有条目错位。patch 时必须按新顺序重写整个布局数据块。
3. **UID 位定义**：bits 24..27 类型；**bit 23 = builtin**（`loader_get_builtin_flag`，179 0x0C6B9172：`uid & 0x800000`，置位则去 `/watchface/builtin/builtin.bin` 查找）；
   bits 19..22 配色索引；bits 0..18 序号。原 marker UID 0x0281BA12 的 bit 23 置位，导致 "Don't find uid"。现用 **0x0207BA12**。
4. 图片按完整 UID 线性查找（`wf_heap_get_image_resource`），渲染时 UID 会 OR 上配色索引，所以有 N 种配色的主题需要 N 条 marker 记录。
5. `decode_image_record` 只接受 RLE 方法 0/1（`flags & 0x18` 非零即拒绝），方法 2 不可用。官方表盘全部是 Indexed8 + RLE 方法 1，marker 也改用这一组合。
6. 主题背景（主题行字 0）由 `render_ground_create` 单独创建；布局循环跳过与背景同 UID 的条目。
7. **触摸**：叠在 marker 上方的组件会拦截触摸。模块在布局容器里建一个透明的顶层代理对象覆盖摸头判定框（清除 SCROLL_CHAIN 与 GESTURE_BUBBLE），
   摸头不会触发表盘翻页；判定框以外的滑动照常交给系统。

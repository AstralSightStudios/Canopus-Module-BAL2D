-- BAL2D 图层示例：Lua 表盘（小米手环 10 Pro 3.101.043）。
--
-- 任何显示 /data/canopus/m.ba_l2d/layer.bin 的 lvgl.Image 都会被 ba_l2d 模块接管，
-- 绘制当前角色。它在对象树中的位置就是角色的层级：之前创建的对象在角色下面，之后创建的
-- 对象（这里的时间）在角色上面。不给尺寸时图层占满全屏。
-- 模块未运行时，这个文件是一张 16x16 的透明图，表盘照常显示。

local lvgl = require("lvgl")

local root = lvgl.Object(nil, {
    w = lvgl.HOR_RES(), h = lvgl.VER_RES(), bg_color = 0x18233A,
    bg_opa = lvgl.OPA(100), border_width = 0, pad_all = 0,
})
root:clear_flag(lvgl.FLAG.SCROLLABLE)

-- 角色图层。
lvgl.Image(root, { src = "/data/canopus/m.ba_l2d/layer.bin", x = 0, y = 0 })

-- 叠在角色上方的时间与日期。
local clock = lvgl.Label(root, {
    text_font = lvgl.Font("MiSans-Regular", 72), text = "--:--", text_color = 0xFFFFFF,
    align = { type = lvgl.ALIGN.TOP_MID, x_ofs = 0, y_ofs = 28 },
})
local date = lvgl.Label(root, {
    text_font = lvgl.Font("MiSans-Regular", 22), text = "", text_color = 0xDDE8FF,
    align = { type = lvgl.ALIGN.TOP_MID, x_ofs = 0, y_ofs = 112 },
})

local function tick()
    clock:set { text = os.date("%H:%M") }
    date:set { text = os.date("%m/%d %a") }
end
tick()
lvgl.Timer { period = 1000, repeat_count = -1, cb = tick }

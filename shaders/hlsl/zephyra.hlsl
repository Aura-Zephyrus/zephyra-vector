#include "common.hlsli"
#include "sdf.hlsli"
#include "shade.hlsli"
#include "blend.hlsli"
#include "blur.hlsli"
#include "refraction.hlsli"
#include "lens.hlsli"
#include "backdrop.hlsli"
#include "decoration.hlsli"

#ifdef ZEPHYRA_NV12
// NV12 变体的主入口。由 `zephyra_nv12.hlsl` 先定义宏再 include 本文件生成，
// 默认编译（不带宏）时这一行不存在 —— PSMain 的字节码逐字节不变。
float4 PSMainNv12(VSOut i) : SV_TARGET { return ShadeSource(i); }
#endif

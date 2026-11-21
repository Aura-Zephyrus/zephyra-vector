// NV12 纹理的像素着色器变体聚合入口。
//
// 与 `zephyra.hlsl` 共用同一批 .hlsli 源，只在 include 之前打开 `ZEPHYRA_NV12`：
// - 图像分支把采样到的 (Y,U,V) 转成直通 RGB（BT.601 limited range）；
// - 所有 PS 入口名带 `Nv12` 后缀，与默认那套并存、互不覆盖。
//
// 生成命令（在本文件所在目录执行）：
//   fxc /nologo /T ps_5_0 /E PSMainNv12      /Fo ..\dxbc\zephyra.PSMainNv12.ps.bin      zephyra_nv12.hlsl
//   fxc /nologo /T ps_5_0 /E PSOverlayNv12   /Fo ..\dxbc\zephyra.PSOverlayNv12.ps.bin   zephyra_nv12.hlsl
//   ...（其余 13 个混合模式入口同名加后缀）
//
// ⚠ 默认 `zephyra.hlsl` 的 25 个入口必须仍用原文件编译 —— 那份字节码是 RGBA
// 路径的回归基准，本文件只新增变体，不替换任何既有产物。
#define ZEPHYRA_NV12 1
#include "zephyra.hlsl"

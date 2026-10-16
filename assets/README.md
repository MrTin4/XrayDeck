# XrayDeck 应用图标

- `xray-deck.png`：内置 image_gen 生成的透明背景原图。
- `xray-deck.ico`：从原图高质量缩放、保留透明度的 Windows 图标，包含 16、20、24、32、40、48、64、128、256 像素的 32 位 PNG 帧。
- `app.rc`：将 ICO 作为资源 ID 1 嵌入 EXE；窗口大小图标和系统托盘共用该资源，不依赖外部文件。

生成提示词（内置工具模式）：

> Use case: logo-brand. Asset type: Windows desktop app icon for XrayDeck, an Xray networking service control panel. Generate one square icon, transparent background outside the rounded-square tile. Subject: a bold geometric abstract X built from two interlocking cyan and ice-blue diagonal bands, with a tiny clean negative-space center suggesting network routing. Style: professional minimal flat vector-like app mark, exceptionally clear at 16 and 32 pixels. Dark navy rounded-square tile (#101827), luminous but flat cyan (#67e8f9) and blue (#60a5fa) X. Centered, tile fills about 90% of square canvas, balanced padding. No text, no letters other than the abstract X, no thin lines, no small dots, no shadows outside the tile, no perspective, no mockup, no watermark. Deliver only the single finished icon.

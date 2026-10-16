# XrayDeck

管理已有 WinSW Xray 服务的 Windows 桌面面板，使用 Rust 和 GPUI Kit 0.7.3 构建。

当前版本 **0.1.21**，已达到首个 MVP 目标。支持 Windows 10/11 x64，以便携包交付。

## 功能

- 通过 gRPC API 读取运行时出站，显示协议、地址与核心已有观测结果，支持延迟和时间排序。
- 手动刷新及可配置的自动刷新（5–86400 秒），保留最近读取结果和独立状态提示。
- 校验 Windows 服务、WinSW XML、API 进程与核心路径关联，通过 `xray.exe --version` 查询关联核心版本。
- 检查 Xray 官方正式版或预发布版；用户确认后下载、校验并通过 UAC 辅助进程升级，失败时尝试回滚。
- 保存连接、服务关联、更新渠道与刷新设置；正常使用无需管理员权限。

## 开始使用

1. 准备正在运行的 WinSW Xray 服务，并启用 `HandlerService` 的明文 gRPC API（默认 `127.0.0.1:8080`）。观测结果需要额外配置 Observatory 模块和服务。
2. 解压 `XrayDeck-0.1.21-windows-x64.zip`，保留 EXE 与随包 DLL 在同一目录，运行 `xray-deck.exe`。
3. 在“设置”中保存并测试 API 地址，返回主页点击“刷新出站”。
4. 如需查询本地核心版本或升级，填写 Windows 服务名、WinSW 同名 XML 和 `xray.exe` 完整路径，完成关联校验。

设置、日志、更新缓存及备份位于 `%APPDATA%\XrayDeck\`。关闭窗口会退出面板，不停止 Xray；升级关键阶段会等待操作结束。

## 能力边界

- 刷新只读取核心已有观测结果，不发起测速。Burst Observatory 未提供真实探测时间时显示“核心未提供”。
- 首版不提供手动分组、节点切换、路由编辑、核心安装或 XrayDeck 自身更新。
- 核心升级仅支持本机回环 API、WinSW 同名 XML 和 Windows x64 核心，会重启服务并中断连接。
- `--version` 查询的是关联核心文件；权限不足时无法直接确认进程映像路径，设置页会注明来源。升级提权后仍须复核实际进程路径。

## 开发

需要 Rust stable（rustfmt、Clippy）、MSVC C++ 工具及 Windows SDK。

```powershell
cargo run --locked
./scripts/test.ps1
./scripts/package.ps1
```

如使用项目内 `.tools/` 工具链，先执行 `. ./scripts/env.ps1`。打包输出到 `dist/`；构建产物和本机配置不进入版本控制。GitHub Actions 执行格式、单元测试、Clippy 和便携包构建，产物保存在对应运行的 Artifacts 中。

## 文档

- [使用、设置与升级恢复](docs/user-guide.md)
- [开启观测 API](docs/enable-observatory.md)
- [开发、目录结构与验证](docs/development.md)
- [MVP 交付记录](docs/mvp-release.md)
- [产品需求与设计约束](docs/product-requirements.md)
- [开发阶段验证记录](docs/development-history.md)
- [版本变更](CHANGELOG.md)

## 许可证

[MIT](LICENSE)。XrayDeck 是独立项目，核心更新包来自 XTLS/Xray-core 官方发布；第三方组件适用各自许可证。

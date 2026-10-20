# 开发与维护

## 项目结构

```text
.github/workflows/ci.yml  Windows 自动验证与便携包构建
AGENTS.md               项目协作约定、版本递增及编译交付要求
assets/                  应用图标原图、多尺寸 ICO 与 Windows 资源定义
build.rs                 构建时将图标资源嵌入 EXE
docs/                    使用、设计、开发及验证记录
scripts/env.ps1          当前 PowerShell 会话的工具链环境
scripts/test.ps1         格式、单元测试、Clippy 与可选本机集成测试
scripts/package.ps1      Release 构建、运行库与文档打包
scripts/build.ps1        版本构建入口，每次编译同时生成 dist 发布包
src/main.rs              桌面及命令行入口、提权辅助进程入口
src/ui.rs                GPUI 界面、后台任务与交互状态
src/tray.rs              Windows 托盘图标、恢复窗口、退出及资源管理器重启恢复
src/single_instance.rs   会话内桌面单例锁、跨进程唤起事件与窗口恢复
src/api.rs               gRPC 能力探测、节点与观测、流量、负载均衡目标解码及排序
src/routing_config.rs    关联 API 进程、读取 WinSW 启动配置中的负载均衡器 tag（用于选中状态）
src/service.rs           WinSW、进程、核心路径与版本关联校验
src/update.rs            官方发布查询、下载校验、升级与回滚事务
src/settings.rs          设置校验与原子保存
src/refresh.rs           自动刷新、定时更新检测调度及倒计时
src/version.rs           版本解析和比较
src/logging.rs           固定应用事件及日志轮换
tests/api_live.rs        显式启用的本机只读集成测试
Cargo.toml / Cargo.lock  应用元数据及固定依赖
```

`.tools/`、`target/` 和 `dist/` 分别保存本地工具、编译缓存和发布包，均被 Git 忽略。运行数据在 `%APPDATA%\XrayDeck\`，不要复制进源码仓库。保留 `Cargo.lock` 以复现应用依赖。

## 环境与构建

在 Windows x64 安装 Rust stable 的 `x86_64-pc-windows-msvc` 工具链、rustfmt、Clippy、Visual Studio C++ 构建工具和 Windows SDK。使用 PowerShell 运行：

```powershell
# 可选：启用项目内已有的隔离工具链；脚本不会安装工具
. ./scripts/env.ps1
cargo run --locked
./scripts/test.ps1
./scripts/build.ps1
```

`env.ps1` 只修改当前会话的环境变量，不修改系统 PATH。每次版本编译通过 `scripts/build.ps1` 调用 `scripts/package.ps1`，固定构建 Windows x64 Release，并自动生成 `dist/XrayDeck-<版本>-windows-x64/` 目录、同名 ZIP 和 `.zip.sha256` 校验文件。便携包包含 EXE、MSVC x64 运行库、README、许可证、变更记录及 docs。版本从 Cargo 元数据读取，保留其他版本，同版本重建时清理其旧目录，避免残留文件进入归档。CI 使用同一入口。直接使用 `cargo run/build` 的开发编译不打包。

## 验证

默认检查不会访问真实 Xray 服务或执行升级。47 项核心单元测试包括本机临时 gRPC Reflection 服务、设置临时文件以及模拟升级事务，另有 3 项桌面列表测试及 3 项单例测试，覆盖跨进程重复启动、启动期间的唤起请求、锁释放和隐藏/最小化窗口恢复。`-Tray` 可额外运行 1 项原生托盘测试，创建独立临时窗口验证隐藏、恢复及图标重建；需要交互式 Windows 桌面和资源管理器，不访问用户设置。测试环境需允许回环连接和临时文件替换。升级及回滚模拟测试不代表真实服务端到端验收。

```powershell
cargo fmt --check
cargo test --locked --no-default-features
cargo test --locked --all-features --bin xray-deck
cargo clippy --locked --all-targets --all-features -- -D warnings

# 可选：访问本机运行核心，仅读取，不修改或重启服务
$env:XRAY_DECK_TEST_API = '127.0.0.1:8080'
./scripts/test.ps1 -Live -ServiceSettings "$env:APPDATA\XrayDeck\settings.json"
# 仅在 Reflection 已开启时使用
./scripts/test.ps1 -Reflection
# 可选：有资源管理器的交互式 Windows 会话，验证原生托盘隐藏、恢复和重建
./scripts/test.ps1 -Tray
```

只读诊断命令使用调试构建输出控制台信息：

```powershell
cargo run --locked --no-default-features -- --inspect 127.0.0.1:8080
cargo run --locked --no-default-features -- --validate-service path/to/settings.json
cargo run --locked --no-default-features -- --check-updates
```

诊断输出可能包含节点地址及本地路径，反馈问题前应脱敏。`--apply-update` 是内部提权辅助入口，不作为手工诊断命令使用。

## 提交与交付

修改业务逻辑时补充对应测试；修改后运行 `scripts/test.ps1`，完成验证后及时递增版本并通过 `scripts/build.ps1` 编译生成新版本便携包，核对归档内容及 SHA-256，不只交付源码。保留旧版本产物。界面改动需人工验证。版本变化时同步 Cargo.toml、Cargo.lock、README、CHANGELOG 及需求文档中的当前交付信息。

提交前检查 `git diff --check`、暂存清单与忽略规则，避免加入用户设置、凭据、核心配置、日志、下载缓存或构建产物。报告问题可使用 GitHub Issues，附应用版本、Windows 版本、复现步骤及脱敏诊断。

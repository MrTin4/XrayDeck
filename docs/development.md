# 开发与维护

## 项目结构

```text
.github/workflows/ci.yml  Windows 自动验证与便携包构建
docs/                    使用、设计、开发及验证记录
scripts/env.ps1          当前 PowerShell 会话的工具链环境
scripts/test.ps1         格式、单元测试、Clippy 与可选本机集成测试
scripts/package.ps1      Release 构建、运行库与文档打包
src/main.rs              桌面及命令行入口、提权辅助进程入口
src/ui.rs                GPUI 界面、后台任务与交互状态
src/api.rs               gRPC 能力探测、节点与观测数据解码、排序
src/service.rs           WinSW、进程、核心路径与版本关联校验
src/update.rs            官方发布查询、下载校验、升级与回滚事务
src/settings.rs          设置校验与原子保存
src/refresh.rs           自动刷新调度及倒计时
src/version.rs           版本解析和比较
src/logging.rs           固定应用事件及日志轮换
tests/api_live.rs        显式启用的本机只读集成测试
Cargo.toml / Cargo.lock  应用元数据及固定依赖
```

`.tools/`、`target/` 和 `dist/` 分别保存本地工具、编译缓存和打包产物，均被 Git 忽略。运行数据在 `%APPDATA%\XrayDeck\`，不要复制进源码仓库。保留 `Cargo.lock` 以复现应用依赖。

## 环境与构建

在 Windows x64 安装 Rust stable 的 `x86_64-pc-windows-msvc` 工具链、rustfmt、Clippy、Visual Studio C++ 构建工具和 Windows SDK。使用 PowerShell 运行：

```powershell
# 可选：启用项目内已有的隔离工具链；脚本不会安装工具
. ./scripts/env.ps1
cargo run --locked
./scripts/test.ps1
./scripts/package.ps1
```

`env.ps1` 只修改当前会话的环境变量，不修改系统 PATH。便携包包含 EXE、MSVC x64 运行库、README、许可证、变更记录及 docs。输出为 `dist/XrayDeck-<版本>-windows-x64.zip`，旁边的 `.sha256` 文件用于验证归档完整性。CI 使用相同打包脚本。

## 验证

默认检查不会访问真实 Xray 服务或执行升级。38 项单元测试包括本机临时 gRPC Reflection 服务、设置临时文件以及模拟升级事务；测试环境需允许回环连接和临时文件替换。升级及回滚模拟测试不代表真实服务端到端验收。

```powershell
cargo fmt --check
cargo test --locked --no-default-features
cargo clippy --locked --all-targets --all-features -- -D warnings

# 可选：访问本机运行核心，仅读取，不修改或重启服务
$env:XRAY_DECK_TEST_API = '127.0.0.1:8080'
./scripts/test.ps1 -Live -ServiceSettings "$env:APPDATA\XrayDeck\settings.json"
# 仅在 Reflection 已开启时使用
./scripts/test.ps1 -Reflection
```

只读诊断命令使用调试构建输出控制台信息：

```powershell
cargo run --locked --no-default-features -- --inspect 127.0.0.1:8080
cargo run --locked --no-default-features -- --validate-service path/to/settings.json
cargo run --locked --no-default-features -- --check-updates
```

诊断输出可能包含节点地址及本地路径，反馈问题前应脱敏。`--apply-update` 是内部提权辅助入口，不作为手工诊断命令使用。

## 提交与交付

修改业务逻辑时补充对应测试；修改后运行 `scripts/test.ps1`，交付前运行 `scripts/package.ps1` 并校验归档内容及 SHA-256。界面改动需人工验证。版本变化时同步 Cargo.toml、Cargo.lock、README 与 CHANGELOG。

提交前检查 `git diff --check`、暂存清单与忽略规则，避免加入用户设置、凭据、核心配置、日志、下载缓存或构建产物。报告问题可使用 GitHub Issues，附应用版本、Windows 版本、复现步骤及脱敏诊断。

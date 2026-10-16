# 开启 Xray 观测结果 API

本指南基于 Xray 26.10.10 的本机验证整理。若面板提示没有注册 `xray.core.app.observatory.command.ObservatoryService`，直接调用可能返回 `Unimplemented: unknown service`。这与未实现方法、网络超时或访问被拒绝是不同情况；历史诊断不代表你的实例当前状态。

## 1. 在实际生效配置中启用 API 服务

在现有 `api.services` 数组中**追加** `ObservatoryService`，保留原有服务、`tag`、`listen` 和 API 路由。例如原有服务列表为 Handler、Logger、Stats、Routing、Reflection 时：

```json
{
  "api": {
    "tag": "api",
    "listen": "127.0.0.1:8080",
    "services": [
      "HandlerService",
      "LoggerService",
      "StatsService",
      "RoutingService",
      "ReflectionService",
      "ObservatoryService"
    ]
  }
}
```

这是配置片段，不是完整核心配置，也不要用此片段覆盖现有文件。应修改 WinSW 实际启动的配置源；使用多文件配置时定位已有 `api` 所在文件。

## 2. 确保核心已有观测模块

只开启 API 不会自动创建观测任务。如果已经存在 `observatory` 或 `burstObservatory`，先保留现有配置，不要额外叠加第二套观测模块，避免影响原来的自动选择逻辑。

如果没有配置观测模块，可以在顶层添加普通 Observatory：

```json
{
  "observatory": {
    "subjectSelector": ["替换为实际出站tag或前缀"],
    "probeUrl": "https://www.google.com/generate_204",
    "probeInterval": "60s",
    "enableConcurrency": true
  }
}
```

`subjectSelector` 按出站 tag 前缀匹配，不是任意显示名称或分组名称。请从 XrayDeck 节点列表中选择实际 tag，并替换示例占位值。建议先只选择一个节点或少量节点，避免对全部节点高频并发探测。探测 URL 可以换成你可通过节点访问、返回小响应的 HTTPS 地址。

## 3. 校验配置并重启

使用 WinSW 相同的配置文件、目录和工作目录，先执行核心配置校验，例如单文件配置：

```powershell
& 'D:\Program Files\xray-core\xray.exe' run -test -config '实际配置文件的完整路径'
```

多文件配置应使用服务原来的 `-confdir` 等参数进行校验。校验通过后，在管理员 PowerShell 中执行：

```powershell
Restart-Service -Name 'winsw-xray'
```

重启会中断现有连接。

## 4. 验证 API

如果已安装 grpcurl，可在命令行只读验证（该工具不包含在便携包中）：

```powershell
grpcurl -plaintext 127.0.0.1:8080 list
grpcurl -plaintext -d '{}' 127.0.0.1:8080 xray.core.app.observatory.command.ObservatoryService/GetOutboundStatus
```

第一条的服务列表应包含完整的 ObservatoryService 名称。仅能查询到该类型的方法描述不能证明服务已注册，必须看实际服务列表或直接调用。

之后在 XrayDeck 点击“刷新出站”。刚重启时可能还没有结果，等待实际探测完成后再次刷新；没有被 `subjectSelector` 覆盖的节点仍显示“暂无数据”。读取结果不会触发立即测速，也不会改变核心探测 URL。

官方依据：[API 服务配置实现（26.10.10）](https://github.com/XTLS/Xray-core/blob/v26.10.10/infra/conf/api.go)、[观测 API 协议](https://github.com/XTLS/Xray-core/blob/main/app/observatory/command/command.proto)、[连接观测文档](https://xtls.github.io/config/observatory.html)。

# 日志管理员

角色 ID 为 `log_analysis`。公共封套、字段规则和权限边界见 [Infinity 协议](../INFINITY.md)。

运行范围是 `{source_id: id, service: string, time_range: TimeRange}`。使用定向日志读取、筛选、分组与分页。固定查询无状态，跨时间窗口关联使用 job 内游标。

| 动作 | `args` | `data` |
| --- | --- | --- |
| `scan` | `event_kinds: (warning \| error \| crash \| restart)[]`、`sample_limit?: int` | `read_windows: TimeRange[]`、`counts: {kind: string, count: int}[]`、`samples: {at: timestamp, kind: string, ref: EvidenceRef}[]` |
| `correlate` | `anchor_refs: EvidenceRef[]`、`window_seconds: int` | `timeline: {at: timestamp, event: string, ref: EvidenceRef}[]`、`unresolved: string[]` |

```text
你负责读取指定服务、来源和时间窗内的事件。先确认可读窗口、时区、水位和轮转缺口。
级别与事件计数使用查询器输出。关联事件时检查来源、时间和重复样本，区分观察到的顺序
与推测的原因。结果包含覆盖窗口、异常计数与有代表性的证据。覆盖不完整时限定结论范围。
```

## 查询与覆盖

接入时使用已注册的日志来源。来源负责权限、服务解析、轮转与时间格式。把“最近一小时”解析为确定的时间窗后再派发，同一次 job 的继续和重试共享该时间窗。每个来源返回实际查询窗口和水位。对于断档、无权限、尚未采集或查询截断，分别报告缺口。

先做确定性提取、去重和分级计数。只有关联崩溃前后的事件或解释非结构化日志时才启动小模型。证据片段按事件引用取回，包含来源与规范化时间。无法证明计数已覆盖整窗时，`scan` 返回 `partial`。平台日志适配器和服务名校验是该角色启用的前置工作。

## 当前实现

当前实现支持项目内文本或 JSONL 日志，以及 Linux 上由界面批准的 `journal` 来源。项目日志需要 `scope.worktree_id` 和可被索引的相对路径 `scope.source_id`。时间过滤优先解析 RFC 3339，journald 使用其实际事件时间和优先级。Windows 和 macOS 可使用项目中的日志文件。扫描最多读取 1 MiB，系统日志最多 500 条。事件关联可从同一个文件补取锚点附近事件。整窗留存无法验证时报告部分覆盖。

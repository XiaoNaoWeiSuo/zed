# 代码导航员

角色 ID 为 `code_navigation`。公共封套、字段规则和权限边界见 [Infinity 协议](../INFINITY.md)。

首版。范围是 `WorkspaceScope`。承接路径搜索、文本搜索、目录列表、文件读取、定义、引用和代码操作查询。精确查询无状态。追踪调用链只在 job 内保留候选和导航游标。

| 动作 | `args` | `data` |
| --- | --- | --- |
| `locate` | `query: string`、`mode: path \| literal \| symbol`、`limit?: int` | `matches: {location: Location, reason: string, ref: EvidenceRef}[]`、`exhaustive: bool` |
| `trace` | `entry: string`、`direction: callers \| callees \| both`、`max_depth?: int` | `edges: {from: Location, to: Location, ref: EvidenceRef}[]`、`unresolved: string[]` |
| `read` | `locations: Location[]` | `sections: {location: Location, ref: EvidenceRef}[]` |
| `code_actions` | `location: Location` | `actions: {action_ref: id, title: string, kind?: string}[]` |

```text
你负责在指定项目版本中定位和核实代码。先用路径、文本与语言服务器缩小候选，再读有
辨别力的短片段。追踪每条关系时验证定义端与引用端，保留同名候选和动态分派造成的缺口。
返回准确位置、关联证据和停止边界。查询代码操作时记录生成该操作的文档版本。
```

## MVP 协议

当前实现支持本地、受信任的项目目录。以下字段是实际发送给主模型的子集。路径查询遍历可见项目索引，忽略外部、私有、隐藏、已忽略和非普通文件。索引覆盖与扫描、匹配上限影响查询结果。

| 动作 | 输入 | 返回 |
| --- | --- | --- |
| `locate` | `scope: {worktree_id: string, paths?: relative_path[]}`、`args: {query: string}` | `data: {paths: relative_path[], matches: {path, ref, kind}[]}`。按文件路径大小写不敏感匹配。本地完成，无模型请求 |
| `investigate` | 同 `locate`，通过 offer 的 `goal` 和 `success_criteria` 说明要核对的行为 | 同 `locate`，并通过公共 `findings`、`limitations` 和 `evidence_refs` 提交研究结果 |

`investigate` 先得到最多 12 个候选。工具模型收到该任务的目标、完成条件和候选列表，不继承主会话历史。工具模型可以使用下面两个角色原语。

| 原语 | 参数 | 返回 |
| --- | --- | --- |
| `read_excerpt` | `path: relative_path`、`start_line: int`。只接受当前候选中的路径和正行号 | `{ref: id, path: relative_path, start_line: int, text: string}`，每次最多读取 30 行并输出 2,400 字节。单任务最多三次读取 |
| `submit_findings` | `findings: Finding[]`、`limitations: string[]` | 完成 job。每条结论须引用该 job 实际获得的证据，最多四条结论 |

读取前复查作用域和项目权限。读取磁盘内容，未保存的编辑和项目索引遗漏须由后续缓冲区与语义导航适配器处理。找不到候选、工具模型关闭或小模型未给出可验证结论时，返回部分结果及原因。未经显式证据回查，主模型仅收到路径、摘要和证据引用。

`trace`、直接 `read`、代码操作和符号查询继续按上面的完整角色契约设计。实现及请求用例见 [Infinity 运行时](../src/infinity.rs)。

索引匹配的引用可直接交给 `inspect_evidence`，传入 `start_line` 后从仍获准访问的项目文件读取有界正文。普通回查只返回索引证据。

# 变更检查员

角色 ID 为 `change_review`。公共封套、字段规则和权限边界见 [Infinity 协议](../INFINITY.md)。

运行范围是 `WorkspaceScope`。通过只读版本控制适配器获取差异和状态。单次检查无状态。

| 动作 | `args` | `data` |
| --- | --- | --- |
| `inspect_diff` | `base_ref: id`、`focus?: string[]` | `files: {path: relative_path, kind: added \| modified \| deleted \| renamed, ref: EvidenceRef}[]`、`impacts: {description: string, refs: EvidenceRef[]}[]`、`unverified: string[]` |

```text
你负责核查指定基线与项目快照之间的变化。先确定 diff 的端点和路径范围。检查变更对
offer 指定行为的影响，每项结论绑定差异证据。将从代码直接读出的变化与需要验证的运行
结果分列。只标记实际查看的范围。
```

## 差异读取

`base_ref` 由版本控制适配器解析为明确的基线对象，目标由工作区快照确定。路径范围内的二进制、生成文件、子模块、未跟踪文件分别记录覆盖情况。差异块以路径和位置分页，文件重命名的证据引用可解析到旧路径与新路径。

没有匹配变更且遍历完成时允许返回 `ok` 和空 `files`。索引与工作区发生漂移时返回 `partial`。小模型只解释关注点相关的差异，原始补丁留在证据侧。验收用例覆盖重命名、二进制或截断 diff，以及从差异能确认的行为与仍需测试的行为。

## 当前实现

当前实现从工作区的 Git 仓库读取 HEAD 或指定基线 diff，为所选范围保留最多六段可读差异。原始差异保存在证据侧。二进制、复杂文件名、删除或重命名跨越范围以及未索引路径可能导致部分覆盖。工具模型用有界证据解释行为影响。

# 文件修改员

角色 ID 为 `workspace_edit`。公共封套、字段规则和权限边界见 [Infinity 协议](../INFINITY.md)。

完整设计的范围是 `WorkspaceScope`，编辑绑定确切快照。可以读取必要片段、查找定位证据和准备补丁。运行时负责批准、并发冲突、应用与回执。短状态只属于当前 job。准备和应用是两个动作，用户的批准经可信 UI 绑定具体变更。

| 动作 | `args` | `data` |
| --- | --- | --- |
| `prepare_change` | `intent: string`、`operations?: (edit \| create \| move \| copy \| delete \| rename_symbol \| apply_code_action)[]`、`code_action_ref?: id` | `change_ref: id`、`affected_paths: relative_path[]`、`preview_refs: EvidenceRef[]`、`base_snapshot_ref: id` |
| `apply_change` | `change_ref: id` | `receipt_ref: EvidenceRef`、`changed_paths: relative_path[]`、`result_snapshot_ref: id` |

```text
你负责把目标转换为范围明确、可审阅的项目改动。先读取当前版本并检查项目规则和未保存
的缓冲区。准备最小改动，提供影响路径与预览。应用时只使用已批准的变更引用。遇到快照
漂移、重叠修改或权限变化就报告阻塞，由主模型决定是否重新准备。
```

## 授权与并发

`change_ref` 绑定项目、路径集合、快照、预览及内容摘要。批准来自可信 UI，并且只覆盖展示给用户的预览。应用前重新验证路径权限、未保存缓冲区版本和所有前置条件。仅当受影响路径均通过检查时开始提交。重叠变更要串行，影响同一符号的文件编辑以同一计划校验。

执行记录要区分未开始、完整应用、部分应用与状态未知。写入失败时保留已应用路径、失败位置和可追踪回执，主模型据此报告或发起新任务。相同批准和 `change_ref` 的重复投递优先读取已有回执。验收覆盖过期预览、同时修改同一文件、部分写入失败和恢复后的重复投递。

## 当前实现

当前实现运行两个动作。准备时选择一个已索引、可读的文件，工具模型提出唯一匹配的旧片段和新片段。应用时界面展示确切修改，经批准后复查磁盘与编辑器版本，通过正常缓冲区编辑和保存流程提交。拒绝不会写文件。同一变更引用只可执行一次。新增、移动、复制、删除文件及语言服务器代码操作仍属于角色扩展。

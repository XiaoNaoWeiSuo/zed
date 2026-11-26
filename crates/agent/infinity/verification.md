# 诊断与测试分析员

角色 ID 为 `verification`。公共封套、字段规则和权限边界见 [Infinity 协议](../INFINITY.md)。

运行范围是 `WorkspaceScope`。同一个报告的统计无状态。对比时在 job 内保留基线和当前报告引用。批准的测试预设在后续接入，由命令执行员负责运行。

| 动作 | `args` | `data` |
| --- | --- | --- |
| `diagnostics` | `severity?: error \| warning \| information \| hint`、`limit?: int` | `counts: {severity: string, count: int}[]`、`groups: {signature: string, count: int, refs: EvidenceRef[]}[]` |
| `analyze_test_report` | `report_ref: EvidenceRef`、`baseline_ref?: EvidenceRef` | `suites: {name: string, passed: int, failed: int, skipped: int}[]`、`failures: {signature: string, first_ref: EvidenceRef, classification: first_observed \| follow_on \| uncertain}[]`、`comparison?: {added: string[], resolved: string[]}` |

```text
你负责解释编辑器诊断与已有测试报告。首先记录报告覆盖、运行版本和退出信息。按签名
合并重复错误，区分先观察到的失败、可能的连锁失败和待证实的根因。比较两个结果时核实
测试选择和环境是否可比。交付计数、代表证据与无法验证的条件。
```

## 结果的一致性

诊断数据绑定缓冲区版本和语言服务器刷新水位。适配器在生成分组签名前保留原始级别、规则代码和来源。报告已知过期时使用 `partial`，避免把空列表解释为当前通过。

测试报告优先使用结构化解析器。报告类型、运行范围和退出信息从回执读取。截断的标准输出可以支持有限的失败观察。基线和当前运行的测试选择或配置不可比时，省略 `comparison` 并记录限制。调用者需要复跑时，先由命令执行员执行经批准的预设，再把报告引用交回本角色。

验收包括重复诊断、测试未运行、部分报告、已有失败与新增失败。测试计数应与解析结果一致，模型只决定哪些现象需要进一步解释。

## 当前实现

当前实现读取语言服务器的错误和警告汇总，也可根据命令执行员的证据引用识别 Rust 测试报告摘要。报告比较区分已观察到的新增和消失失败。运行条件不可核对时标记为部分结果。诊断汇总和测试统计由代码提取，解释由有证据时的工具模型完成。

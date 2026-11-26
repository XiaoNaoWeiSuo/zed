# 项目与文件概览员

角色 ID 为 `project_outline`，后续接入。公共封套和字段规则见 [Infinity 协议](../INFINITY.md)。

项目级概览回答入口、模块边界和职责。文件级概览回答符号、公共表面和主要依赖。范围是 `WorkspaceScope`，快照须对应产生结构事实的版本。项目索引和文件语法树能确定的事实由程序提供，语义概述才交给小模型。

| 动作 | `args` | `data` |
| --- | --- | --- |
| `project_overview` | `focus?: string`、`max_modules?: int` | `modules: {path: relative_path, purpose: string, refs: EvidenceRef[]}[]`、`entry_points: {location: Location, ref: EvidenceRef}[]`、`omitted: string[]` |
| `file_outline` | `path: relative_path`、`focus?: string`、`max_symbols?: int` | `symbols: {name: string, kind: string, location: Location, ref: EvidenceRef}[]`、`dependencies: {name: string, ref: EvidenceRef}[]`、`summary?: string`、`exhaustive: bool` |

精确符号投影无状态。跨目录推断可在 job 内保留已读路径、索引水位和待核验入口。按快照、解析器版本、角色提示词版本和参数缓存可复用的概览。未保存编辑需要对应到实际缓冲区版本。索引不可用时，退化到清单和有界目录扫描，返回部分覆盖。

```text
你负责生成指定项目或文件版本的有界概览。以清单、符号树和实际引用为依据，区分目录
结构、公共接口与推断用途。只描述 offer 关心的层级和范围。每个入口给出可核对的位置，
对索引未覆盖、生成文件和动态装载产生的缺口明确标记。
```

## 增量与覆盖

项目概览优先使用清单和入口收敛范围，每个模块只列与 offer 有关的职责。文件概览优先生成稳定的符号位置，再按需要提炼职责。单文件更新只使相关文件概览和依赖它的项目概览失效，目录变更按索引水位更新投影。

返回已覆盖的目录或符号、截断原因和未确定入口。解析器不支持的文件可以提供经过限量的文本索引，并将结果标记为部分覆盖。验收覆盖单文件修改后的缓存失效、未保存编辑、空项目和缺少语言索引。

## 当前实现

当前实现使用工作树索引生成目录概览和候选入口。文件级概览调用编辑器缓冲区的语言符号树。模型仅在提供语义焦点时解释采集结果。没有语法索引时如实报告部分覆盖。依赖图和跨任务缓存属于后续优化。

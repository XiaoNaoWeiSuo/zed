---
title: Agent
description: Use Lin's native AI agent with Lin-configured models, tools, profiles, skills, instructions, and MCP servers.
---

# Agent

Agent is the native agent path. It runs in the [Agent Panel](./agent-panel.md) and [Threads Sidebar](./parallel-agents.md#threads-sidebar), uses models configured through [LLM Providers](./llm-providers.md), and integrates with Lin's project, editor, terminal, and review surfaces.

Use Agent when you want the agent to:

- read and search your project
- edit files
- run terminal commands
- use Lin-managed MCP tools
- follow [Agent Profiles](./agent-profiles.md)
- use Lin [Skills](./skills.md) and [Instructions](./instructions.md)
- show changes in Lin's review UI

## What Agent Uses {#what-lin-agent-uses}

| Capability                 | Source of truth                           |
| -------------------------- | ----------------------------------------- |
| Model access               | [LLM Providers](./llm-providers.md)       |
| Panel workflow             | [Agent Panel](./agent-panel.md)           |
| Tool availability          | [Agent Profiles](./agent-profiles.md)     |
| Tool approval behavior     | [Tool Permissions](./tool-permissions.md) |
| Built-in tools             | [Tools](./tools.md)                       |
| External tools             | [MCP](./mcp.md)                           |
| Reusable task instructions | [Skills](./skills.md)                     |
| Always-on instructions     | [Instructions](./instructions.md)         |

## How It Differs from Other Agent Paths {#other-agent-paths}

| Agent path                                | Main difference                                                                              |
| ----------------------------------------- | -------------------------------------------------------------------------------------------- |
| [Agent](./agent.md)               | Uses Lin's model, tool, profile, skill, instruction, and MCP configuration                   |
| [External Agents](./external-agents.md)   | Use an ACP integration and often own auth, model, tool, and native instruction configuration |
| [Terminal Threads](./terminal-threads.md) | Run a CLI/TUI in a terminal-backed thread; the CLI owns auth and configuration               |

See [Agents](./agents.md) for the full comparison.

## Goals and completion evidence {#goals}

For sustained work, explicitly ask Agent to pursue a goal with observable success criteria. The `manage_goal` tool keeps the objective, verification surfaces, constraints, lifecycle, and completion evidence with the thread. The panel displays the goal and supports pausing and resuming it. Reopening a thread pauses an active goal until you resume it.

For current-workspace claims, Agent uses `manage_goal` with `action: "verify"` to execute an enabled read, search, or terminal tool with declared source-file or directory dependencies. The original tool's profile restrictions, permission rules, and sandbox still apply. A receipt records the actual tool input, output fingerprint, dependency-content fingerprints, result, and observation time.

Dependencies are captured before and after execution. A failed tool, changing input, missing dependency, or unsaved dependency cannot establish a passing verification. Directories include their membership, so adding or deleting a file changes the fingerprint. Private/excluded files and paths escaping the project cannot be used to bypass file-access rules. Scans are bounded to 32 declared roots, 10000 entries, and 64 MiB; exceeding a limit fails explicitly rather than certifying partial coverage.

For terminal checks, the receipt includes the process exit code and whether the command timed out or was stopped. Only an observed zero exit code without interruption passes; a missing exit status does not. Large verification output is shortened in model context, while the retained original is available through `inspect_tool_result`.

Before completion, Agent assesses every criterion and cites verification receipt IDs. The harness rereads their declared dependencies and rejects stale or failed receipts. Changes outside those dependencies do not invalidate a check. `action: "inspect"` rechecks receipt freshness without rerunning the original tools; the default view shows five recent summaries, and `verification_id` selects an older receipt. Receipts persist with the goal.

Evidence scopes are fixed at goal creation: each criterion defaults to `workspace`. A goal can explicitly declare `historical` scope for observations independent of the current workspace. Those criteria may cite original completed tool calls instead, but must set `historical: true` and state their limitations when completing. A workspace criterion cannot be downgraded to historical evidence after a check fails. Missing, failed, empty, or bookkeeping-only observation sources are rejected. These limits remain attached to the completion evidence and appear in the goal details.

The `inspect_tool_result` tool lists retained calls and retrieves their input and output in bounded pages. The default Write and Ask profiles enable it and `manage_goal`; verification also requires the underlying tool to be enabled in the selected profile.

The dependency set is an explicit coverage contract, not an automatically discovered build graph. Agent must include relevant source, manifests, configuration, and fixtures. Freshness describes the last audit, not a continuous watch or an atomic guarantee against later edits. A passing tool and unchanged declared files establish execution and local-input freshness, not semantic correctness, environment equivalence, or external-service state. Agent still has to audit those conditions against the user's requirements.


## Project handoffs {#project-handoffs}

Native Agent can save a concise project handoff with `project_checkpoint`. It stores the summary, unresolved work, decisions and next steps in `.lin/checkpoints.sqlite3` under the project root. The default Write and Ask profiles enable the tool. In a multi-root workspace the tool selects an explicitly named root. Agent can list saved work areas with the `list` action and then read a relevant scope; the index is paginated and does not assert that a summary is current.

Saving returns a compact receipt with the checkpoint ID, scope and file/evidence counts instead of repeating the summary and evidence list. The full record remains available through the `read` action and the handoff card.

The editor records available changed buffers, a Git status snapshot and references to recent tool results. Evidence descriptions distinguish tool errors from returned results; a returned terminal result may report a nonzero exit, timeout or interruption and does not imply command success. It also saves an automatic record when a turn involving edits or terminal activity finishes or is interrupted. An automatic record contains observed facts; it does not replace a model-written handoff. An unsaved buffer or a file that could not be read requires further checking.

When a native session opens, Lin loads the latest handoff and the latest outstanding automatic record. The **Project handoff** card expands to show the available summary and changes. Agent receives a compact version at the start of a turn and after context compaction. The handoff files and up to ten files in an additional automatic record are checked against their saved fingerprints; unsaved or unreadable files have an unknown state. Git status comes from the editor's most recent scan and can lag. Project configuration files in `.lin` are included when reported by Git; the checkpoint database and its SQLite sidecars are excluded to avoid tracking the handoff's own writes. Fingerprinting is limited to regular files of at most 8 MiB; larger or non-regular files have an unknown state rather than blocking handoff loading. Unrecorded files, other branches, external services and test coverage still require appropriate verification.

The database can contain project paths and summaries. It is stored inside the project, so configure Git ignore rules if you do not want to commit it. Project handoffs currently require a local trusted workspace; external agents manage their own session state.

To inspect a saved observation, open it from the **Project handoff** card, or have Agent pass its `checkpoint_source` and `checkpoint_scope` to `inspect_tool_result`. Long results are shown in pages. Lin checks that the checkpoint references the observation and that the saved session belongs to an open project root. New checkpoints also record a digest of the tool input and result; a changed result is rejected, while older records without a digest are marked unverified. The original result is still stored with the native session; if that session is deleted or its retained history changes, the observation may no longer be available.

## Questions during a run {#questions-during-a-run}

Agent can use `ask_user` to collect a batch of single-choice, multiple-choice, and text answers. A deferred question leaves the form open while Agent continues independent work. The reply is added once to the conversation at the next model boundary; if Agent has become idle, a reply can resume it without another prompt. A reply does not interrupt a running request or take priority over a new user message. Stopping a run cancels its outstanding forms and prevents an automatic restart.

Deferred forms and their replies are saved with native sessions. Pending forms are recreated when a thread is reopened. Answers retain their place before later user instructions and remain available after context summarization. Agent can retrieve or wait for a saved answer using its request ID, or withdraw an obsolete question with `request_id` and `withdraw: true`. Withdrawal wakes waiters and removes the previous answer from future request history. Rewinding past a question's owning prompt invalidates it and ignores late replies.


## Conversation detail {#conversation-detail}

The `agent.message_detail` setting and the conversation's Compact/Detailed selector control activity disclosure. Compact groups completed tool calls and explicitly marked activity; failures and pending user decisions stay visible. Individual activity blocks can be expanded, and search exposes their full text. Detailed shows activity directly.

Message purposes are structural metadata rather than guesses from punctuation or wording. ACP messages can set `meta.message_purpose` to `progress`, `response`, `activity`, `decision`, or `error`. Unknown or absent values default to a visible response. Native OpenAI Responses messages preserve `commentary` as progress and `final_answer` as a response, including across saving and replay. Providers without message-stage information retain visible text. Thinking content remains separate from user-facing progress.

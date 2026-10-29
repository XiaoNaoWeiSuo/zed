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

Before completing a goal, Agent must assess every criterion and cite completed tool calls from the retained conversation. Missing sources, failed calls, empty observations, and calls that only manage goals or retrieve results cannot be used as completion evidence. The saved evidence includes the source call ID, tool name, and a SHA-256 fingerprint of its input and result.

The `inspect_tool_result` tool lists retained calls and retrieves their input and output in bounded pages, so Agent can reuse observations without rerunning commands. Both tools must be enabled in the selected profile; the default Write and Ask profiles enable them.

Source validation establishes that an observation exists. Agent still has to check whether it covers the criterion and remains relevant to the current project. A successful command or an output fingerprint alone does not prove correctness or detect later workspace changes.


## Questions during a run {#questions-during-a-run}

Agent can use `ask_user` to collect a batch of single-choice, multiple-choice, and text answers. A deferred question leaves the form open while Agent continues independent work. The reply is added once to the conversation at the next model boundary; if Agent has become idle, a reply can resume it without another prompt. A reply does not interrupt a running request or take priority over a new user message. Stopping a run cancels its outstanding forms and prevents an automatic restart.

Deferred forms and their replies are saved with native sessions. Pending forms are recreated when a thread is reopened. Answers retain their place before later user instructions and remain available after context summarization. Agent can retrieve or wait for a saved answer using its request ID, or withdraw an obsolete question with `request_id` and `withdraw: true`. Withdrawal wakes waiters and removes the previous answer from future request history. Rewinding past a question's owning prompt invalidates it and ignores late replies.


## Conversation detail {#conversation-detail}

The `agent.message_detail` setting and the conversation's Compact/Detailed selector control activity disclosure. Compact groups completed tool calls and explicitly marked activity; failures and pending user decisions stay visible. Individual activity blocks can be expanded, and search exposes their full text. Detailed shows activity directly.

Message purposes are structural metadata rather than guesses from punctuation or wording. ACP messages can set `meta.message_purpose` to `progress`, `response`, `activity`, `decision`, or `error`. Unknown or absent values default to a visible response. Native OpenAI Responses messages preserve `commentary` as progress and `final_answer` as a response, including across saving and replay. Providers without message-stage information retain visible text. Thinking content remains separate from user-facing progress.

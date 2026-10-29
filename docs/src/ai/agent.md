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

---
title: AI Privacy - Lin
description: Understand where AI requests go and what data leaves your machine when using AI features in Lin.
---

# AI Privacy

Lin has no hosted AI service. Every AI request goes directly from your machine to a
provider that you configure, or to a local model server. Lin does not proxy prompts,
code context, or responses, and it does not collect feedback or training data.

This page covers [the native agent](./agents.md), [Edit Prediction](./edit-prediction.md),
[Inline Assistant](./inline-assistant.md), and
[Git commit generation](../git.md#ai-support-in-git).

## AI Request Paths {#ai-request-paths}

| Path                                                         | Who handles model requests                      | What to know                                                                                                                                                       | Details                                                                                           |
| ------------------------------------------------------------ | ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------- |
| [Provider API keys](./use-api-access.md)                     | The configured provider                         | The provider handles requests under its own terms. Provider keys are stored in the system keychain, not in `settings.json`.                                        | [Use API Access](./use-api-access.md)                                                             |
| [Gateways](./use-a-gateway.md)                               | The configured gateway and upstream providers   | The gateway and upstream providers handle requests under their own terms.                                                                                          | [Use a Gateway](./use-a-gateway.md)                                                               |
| [Local models](./use-a-local-model.md)                       | The local server or self-hosted endpoint        | The local server handles requests according to how you configured that server.                                                                                     | [Use a Local Model](./use-a-local-model.md)                                                       |
| [External Agents](./external-agents.md)                      | The External Agent and its configured providers | The External Agent handles model requests under its own terms. Tool and MCP behavior depends on agent and ACP configuration.                                       | [External Agents](./external-agents.md)                                                           |
| [Terminal Threads](./terminal-threads.md)                    | The CLI or TUI running in the terminal          | The CLI or TUI owns its auth, model routing, tools, instructions, MCP configuration, and data handling.                                                            | [Terminal Threads](./terminal-threads.md)                                                         |
| [Edit Prediction](./edit-prediction.md)                      | The selected edit prediction provider           | Each keystroke can send local editing context to the selected provider. The provider follows its own terms.                                                        | [Edit Prediction](./edit-prediction.md)                                                           |
| [Agent tools](./tools.md), [MCP](./mcp.md), and integrations | Lin, configured MCP servers, and external systems | Tools can read, edit, search, run commands, fetch URLs, or call external systems depending on profile, MCP server, and tool permission settings.                   | [Agent Profiles](./agent-profiles.md), [Tool Permissions](./tool-permissions.md), [MCP](./mcp.md) |
| Project trust and instructions                               | Lin and the trusted worktree                     | Project-local instructions and skills are loaded from trusted worktrees. External Agents and Terminal Threads may read their own instruction files.                | [Worktree Trust](../worktree-trust.md), [Skills](./skills.md), [Instructions](./instructions.md)  |

## Local Storage

Lin writes agent threads, settings, and logs to its own data directory. Telemetry and
crash reporting are disabled in this build, and nothing is uploaded to Lin.

## Controls

- [AI Quick Start](./quick-start.md#turn-ai-off): How to turn AI off.
- [Worktree Trust](../worktree-trust.md): Which projects are allowed to load local
  instructions and skills.

---
name: Assignment gap causes idle agents
description: Agents never look for unassigned work — CEO must explicitly assign all tasks or they sit idle
type: feedback
---

Agents never look for unassigned work. If a task has no `assigneeAgentId`, no agent will pick it up regardless of priority or status.

**Why:** Two incidents on 2026-03-16 — board flagged work stoppage in DIS-10 because backlog was left unassigned after the initial sprint, and again when 5 new board-created issues sat idle.
**How to apply:** On every heartbeat, query for unassigned issues (`assigneeAgentId` is null) and route them to the right agent before exiting. This is a CEO responsibility — treat it as step 0 of the heartbeat.

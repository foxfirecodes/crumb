# 🍞 Crumb

A personal AI assistant that follows the breadcrumb trail across your work tools — surfacing what matters, tracking decisions, and keeping you oriented.

Crumb connects to **Notion**, **Discord**, and **Asana**, pulls context on demand, and uses AI to give you a live, digestible view of your world.

## What It Does

- **Surfaces what's relevant** — summarizes recent activity, open tasks, and pending decisions across all three sources
- **Answers questions about project state** — "what did we decide about X?" / "what's blocking the launch?" / "what's left on my plate?"
- **Push notifications** — alerts you when important things change (new decisions, blockers, assignments)
- **Breadcrumb trail** — connects related items across tools (an Asana task linked to a Notion doc discussed in a Discord thread)

## How You Use It

### macOS Menubar App (Tauri)

- Quick-glance dropdown showing your current priorities, recent decisions, and action items
- Search/ask bar for natural language queries about project state
- Notification center for important updates

### Discord Bot

- Slash commands to query project state from within Discord
- Summarize threads, surface decisions, and cross-reference with Notion/Asana
- "@crumb what's the status of [project]?" style interactions

Both interfaces are equal citizens — same data, same AI, different surfaces.

## Architecture

```
┌─────────────┐   ┌─────────────┐
│  Tauri App  │   │ Discord Bot │
└──────┬──────┘   └──────┬──────┘
       │                 │
       └────────┬────────┘
                │
         ┌──────┴──────┐
         │  Crumb Core │  ← AI layer (Claude API)
         └──────┬──────┘
                │
    ┌───────────┼───────────┐
    │           │           │
┌───┴───┐ ┌────┴────┐ ┌────┴───┐
│ Notion │ │ Discord │ │ Asana  │
│  API   │ │   API   │ │  API   │
└────────┘ └─────────┘ └────────┘
```

## Data Sync

- **v1: On-demand** — fetches fresh data when you ask
- **Later: Polling** — periodic background sync with local cache for faster responses and proactive notifications

## Tech Stack

- **Menubar app**: Tauri (Rust + web frontend)
- **Discord bot**: Node.js / discord.js
- **AI**: Claude API (Anthropic)
- **Data sources**: Notion API, Discord API, Asana API

## Scope

Personal tool — built for one person's workflow. Not designed for team deployment (yet).

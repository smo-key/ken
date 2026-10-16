---
title: "{{Dependency Name}}"
aliases: ["{{Dependency Name}}", "{{the name people say for it}}", "which version of {{dependency}} are we on"]
status: current
updated: {{date}}
sources:
  - "{{repo@sha:path where the version is pinned, or of the dependency's source}}"
---

# {{Dependency Name}}

What {{dependency}} is, the version we pin, how it behaves where that surprises people, and where its source lives. Point into its source by path; do not restate it.

[[#Purpose]] · [[#Version]] · [[#Quirks]] · [[#Updating]]

## Purpose

{{One paragraph: what the dependency is, what we use it for, and which of our repos depend on it.}}

## Version

| what | value |
|---|---|
| version pinned | {{version}} |
| pinned in | {{repo:path where the version is set}} |
| source | {{reference repo:path, or the URL of the published source}} |

## Quirks

| behaviour | evidence | what we do |
|---|---|---|
| {{what it does that a newcomer would not expect}} | {{repo:path:line, or the test that shows it}} | {{how we work around it}} |

## Updating

{{How a new version is adopted, and what a failure straight after an update usually means.}}

---
title: "{{Dependency Name}}"
aliases: ["{{Dependency Name}}", "{{the name people say for it}}", "which version of {{dependency}} are we on", "where is the {{dependency}} source"]
status: current
audience: dev
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: {{commit}}         # the commit the sources were read at
sources:
  - "{{reference repo:path of its source, or our repo:path where the version is pinned}}"
---

# {{Dependency Name}}

What {{dependency}} is, the version we pin, how it behaves where that surprises people, where its content lives and how an update lands. Not here: our code that calls it → [[{{Repo}} Architecture]]; its words for our things → [[Vocabulary]]; each change we shipped → [[Releases]].

[[#Purpose]] · [[#Version]] · [[#Content Locations]] · [[#Quirks]] · [[#Update Procedure]] · [[#Update History]]

## Purpose

{{One paragraph: what the dependency is, what we use it for, and which of our repos depend on it.}}

## Version

| what | value |
|---|---|
| version pinned | {{version}} |
| pinned in | {{repo:path where the version is set}} |
| source commit | {{the reference repo commit we read against, or none}} |
| licence | {{the licence, and anything it forbids us to do with the source}} |

## Content Locations

| content | lives in | how to read it |
|---|---|---|
| source | {{reference repo:path, or the URL of the published source}} | {{read-only · search with the code map}} |
| documentation | {{URL or path}} | {{which parts are accurate, checked against the source}} |
| data and assets | {{the path, archive or package that holds them}} | {{the tool or command that opens them}} |
| config | {{path}} | {{who reads it and when}} |
| logs | {{path}} | {{what to look for}} |

## Quirks

| behaviour | evidence | what we do |
|---|---|---|
| {{what it does that a newcomer would not expect}} | {{reference repo:path:line, or the test that shows it}} | {{the way we work around it, or the convention page}} |

## Update Procedure

1. {{where a new version is announced}}
2. {{who reads its changes against our code, and where the review is written}}
3. {{the command or file change that moves the pin}}
4. {{the suites and checks that run after, and what a failure straight after an update usually means}}

## Update History

| version | date | what changed for us | ticket |
|---|---|---|---|
| {{version}} | {{date adopted}} | {{what broke, what it allows now, or nothing}} | {{ticket id}} |

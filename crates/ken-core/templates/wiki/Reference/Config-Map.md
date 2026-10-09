---
title: "Config Map"
aliases: ["Config Map", "config reference", "where is this configured", "what does this setting do", "what is the default"]
status: current
audience: dev
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: {{commit}}         # the commit the sources were read at
sources:
  - "{{repo:path of each config file}}"
  - "{{repo:path of the loader that reads it}}"
---

# Config Map

Every config file and key, with its default at the pinned commit, what reads it and what it changes; the live file holds the value in use. Not here: where config files go and how a new one is added → [[Data and Config]]; the platform's own settings → its page on [[Platform]].

[[#{{Config File}}]]

## {{Config File}}

`{{repo:path}}` · read by {{repo:path of the loader}} · {{loaded at start · reloaded by a command · built in}}.

| key | default | read by | changes |
|---|---|---|---|
| {{key}} | {{the default at the pinned commit}} | {{repo:path:line}} | {{what a user or the system does differently}} |

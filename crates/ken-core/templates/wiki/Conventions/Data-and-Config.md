---
title: "Data and Config"
aliases: ["Data and Config", "where do data files go", "where is config kept", "how do I add a config file", "how is config loaded"]
status: current
audience: dev
lens: conventions       # the reviewer that reads this page (presets.json lenses)
updated: {{date}}
verified:               # the date a person read this page against its sources · empty until then · never stamped in bulk
pin: {{commit}}         # the commit the sources were read at
sources:
  - "{{repo:path of the data folder}}"
  - "{{repo:path of the config loader}}"
---

# Data and Config

Where data and config files live, the shape they take, how they are loaded and how a new one is added. Not here: each config key and its default → [[Config Map]]; the platform's own config → its page on [[Platform]]; secrets → {{where secrets are kept}}.

[[#Layout]] · [[#Rules]] · [[#New Files]]

## Layout

| kind of file | lives in | format | loaded by |
|---|---|---|---|
| {{config · seed data · content data · fixtures · migrations}} | {{repo:path}} | {{JSON · YAML · TOML · SQL · …}} | {{repo:path of the loader, and when it runs: start · reload · build}} |

## Rules

| rule | why | checked by |
|---|---|---|
| {{a rule about naming, shape, defaults, validation or reload}} | {{the incident or the cost}} | {{the reviewer, or the check and its command}} |

## New Files

1. {{where the file goes and how it is named}}
2. {{the loader or registry it is added to}}
3. {{the schema or validation it must pass}}
4. {{the row it gets on [[Config Map]] or [[Registries]]}}

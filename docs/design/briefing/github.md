repo: Stingbro/ken
branch: knowledge-layer
path: src

## Last sync
date: 2026-10-01T21:35:19Z

### Updated in this project
- New "Briefing" design system (light + dark)
- Clickable prototype: Home, Inbox, Files, Ingest, Explore, Settings, Ask Ken
- Inbox merges Review + Families; Explore merges Map + Timeline; Record moves into Ingest
- Before/after comparison against the recreated current Home

## Screen map
| Screen | Repo files |
|---|---|
| Ken Current Home.dc.html | src/screens/HomeScreen.svelte, src/shell/Shell.svelte, src/shell/TitleBar.svelte, src/shell/NavRail.svelte, src/lib/ui/KenMark.svelte, src/files/FileGlyph.svelte, src/lib/format.ts, src/app.css |
| Ken Home Directions.dc.html | src/screens/HomeScreen.svelte, src/screens/ReviewScreen.svelte, src/ingests/IngestView.svelte, src/shell/*.svelte |
| Ken Feature Map.dc.html | src/screens/ReviewScreen.svelte, src/ingests/IngestView.svelte, src/lib/ingestCard.ts, src/family/FamilyTray.svelte, src/screens/TeamScreen.svelte, src/screens/RecordScreen.svelte, src/shell/NavRail.svelte |
| Ken Design System.dc.html | src/app.css, docs/design/design-tokens.md, src/lib/format.ts |
| Ken App.dc.html | src/screens/HomeScreen.svelte, src/screens/ReviewScreen.svelte, src/family/FamilyTray.svelte, src/ingests/IngestView.svelte, src/screens/RecordScreen.svelte, src/screens/TeamScreen.svelte, src/screens/TimelineScreen.svelte, src/shell/*.svelte |
| Ken Before After.dc.html | Ken Current Home.dc.html, Ken App.dc.html |

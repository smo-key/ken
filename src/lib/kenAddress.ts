// `ken://` address parsing + resolution (ken-memory task 4.3). Every memory
// and journal file is addressable as `ken://workspace/<rel-path>` (design
// D3's reserved literal host for the `.ken-workspace/` pseudo-member) or
// `ken://<project-id>/<rel-path>` for a real workspace member (design D4).
//
// Real-member addresses resolve exactly like `SearchOverlay`/
// `EntityWikiPanel`'s existing pointer-open flow: switch focus if needed,
// then `app.openInFiles`. `ken://workspace/...` addresses do NOT resolve
// today — see `openKenAddress`'s doc comment for why, and the final report
// for the follow-up this leaves.
import { app } from "./app.svelte";

/** The workspace pseudo-member's reserved address host (design D3) — a
 *  literal string, never the pseudo-member's actual derived uuid. */
export const WORKSPACE_HOST = "workspace";

export interface KenAddress {
  /** `"workspace"` (the pseudo-member's reserved literal) or a real
   *  member's project id. */
  projectId: string;
  relPath: string;
}

const PREFIX = "ken://";

/** Parse a `ken://<host>/<rel-path>` address. `null` for anything that
 *  isn't well-formed (no `ken://` prefix, empty host, or empty rel-path). */
export function parseKenAddress(address: string): KenAddress | null {
  if (!address.startsWith(PREFIX)) return null;
  const rest = address.slice(PREFIX.length);
  const slash = rest.indexOf("/");
  if (slash <= 0 || slash === rest.length - 1) return null;
  return { projectId: rest.slice(0, slash), relPath: rest.slice(slash + 1) };
}

/** Build a `ken://workspace/<rel-path>` address from a workspace-relative
 *  path (e.g. distill-candidate `sources`, which the distillation prompt
 *  instructs the model to emit as plain paths like `journal/2026-07-24.md`,
 *  not full `ken://` URIs — see `compose_distill_prompt` in
 *  `crates/ken-core/src/memory.rs`). No-ops if already a `ken://` address. */
export function toWorkspaceAddress(relPathOrAddress: string): string {
  return relPathOrAddress.startsWith(PREFIX)
    ? relPathOrAddress
    : `${PREFIX}${WORKSPACE_HOST}/${relPathOrAddress}`;
}

/** Why `address` can't be opened right now, or `null` if it can. Drives a
 *  disabled state + tooltip (same honest-disabled pattern as
 *  `EntityWikiPanel.pointerTitle`) rather than attempting a broken open. */
export function unopenableReason(address: string): string | null {
  const parsed = parseKenAddress(address);
  if (!parsed) return "Not a valid ken:// address.";
  if (parsed.projectId === WORKSPACE_HOST) {
    return "Workspace memory/journal files can't be opened from here yet — see the note in src/lib/kenAddress.ts.";
  }
  return null;
}

/**
 * Resolve and open a `ken://` address, matching `SearchOverlay.openHit`'s
 * focus-then-open order for real members.
 *
 * `ken://workspace/...` (the `.ken-workspace/` pseudo-member) is NOT
 * resolvable from the frontend today, by design-collision rather than an
 * oversight:
 *
 * - The pseudo-member is deliberately never added to the workspace
 *   manifest's `members` (design D3: "never shown in the member list"), so
 *   `focus_project`/`focusMember`'s only lookup path for a NOT-YET-resident
 *   id (`ws.member_root(id)`, backend `focus_member_inner`) can never find
 *   it — activation of the pseudo-member happens once, on workspace open
 *   (task 2.1), not via focus.
 * - Forcing focus onto it while it IS resident (it always is, once a
 *   workspace with `kenMemory` on has been opened) doesn't error — it's a
 *   `contains_key` hit — but `WorkspaceOverviewDto.members` is built from
 *   `ws.ws.members` alone, which excludes it, so
 *   `AppStore.loadFocusedMemberState` finds no matching roster entry and
 *   nulls out `app.project`, breaking every screen that reads it (Files,
 *   Settings, …). "Technically doesn't throw" is worse than an honest
 *   failure here, so this never calls `focusMember` for it.
 * - No read/open command accepts an explicit project id to bypass focus
 *   instead: `read_file`/`read_file_bytes`/`open_external` all resolve via
 *   `resolve_path`, which is hardcoded to `member(&guard, None)` — the
 *   CURRENTLY FOCUSED member only.
 *
 * Follow-up (out of this task's src/-only, no-backend-edits scope): a
 * `read_file`-style command taking an explicit project id, or a dedicated
 * "open workspace memory file" command, would let this resolve for real
 * without a focus detour.
 */
export async function openKenAddress(
  address: string,
): Promise<{ ok: true } | { ok: false; reason: string }> {
  const reason = unopenableReason(address);
  if (reason) return { ok: false, reason };
  // unopenableReason already validated this parses and isn't the workspace
  // host, so this is always a real member address past this point.
  const parsed = parseKenAddress(address)!;
  if (parsed.projectId !== app.focused) {
    await app.focusMember(parsed.projectId);
  }
  app.openInFiles(parsed.relPath);
  return { ok: true };
}

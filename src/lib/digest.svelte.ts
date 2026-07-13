// Today's-digest state for the Home card: the parsed row, whether a
// generation is running, and whether Claude is around to write one.
import { api, type DigestDto } from "./api";

class DigestStore {
  digest = $state<DigestDto | null>(null);
  generating = $state(false);
  error = $state<string | null>(null);
  /** Optimistic until claude_doctor answers. */
  claudeFound = $state(true);

  async init() {
    await api.onDigestGenerating(() => {
      this.generating = true;
      this.error = null;
    });
    await api.onDigestUpdated((digest) => {
      this.digest = digest;
      this.generating = false;
      this.error = null;
    });
    await api.onDigestError((message) => {
      this.generating = false;
      this.error = message;
    });
    // Re-read after (re)scans — also covers switching projects.
    await api.onIndexUpdated(() => void this.refresh());
    this.claudeFound =
      (await api.claudeDoctor().catch(() => null))?.found ?? false;
    await this.refresh();
  }

  async refresh() {
    if (!(await api.currentProject().catch(() => null))) return;
    this.digest = await api.currentDigest().catch(() => null);
  }

  /** Force-write today's digest ("Write it now"). */
  async writeNow() {
    this.error = null;
    this.generating = true;
    try {
      await api.refreshDigest();
    } catch (e) {
      this.generating = false;
      this.error = String(e);
    }
  }
}

export const digest = new DigestStore();

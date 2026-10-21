/**
 * App-startup soft probe for catalog ACP agents.
 *
 * Runs independently of the Agent sidebar (which is lazy-mounted). Host emits
 * `agent:registry-changed` after each probe so a later-mounted panel refreshes.
 */
import { probeCatalogAgent, scanCatalog } from "@/lib/agent/api";
import { hasHostTransport } from "@/lib/core/tauri";

let started = false;

/**
 * Scan PATH/catalog once, then ACP-initialize agents that are installed but not
 * yet ready. Skips already-ready rows; idempotent across Strict Mode remounts.
 */
export function prefetchAgentCatalog(): void {
	if (!hasHostTransport() || started) return;
	started = true;
	void (async () => {
		try {
			const scan = await scanCatalog();
			const candidates = scan.entries.filter(
				(e) =>
					e.acpCommandAvailable &&
					(e.acpStatus === "not-probed" || e.acpStatus === "failed"),
			);
			if (candidates.length === 0) return;
			await Promise.allSettled(
				candidates.map((entry) =>
					probeCatalogAgent(entry.templateId).catch(() => null),
				),
			);
		} catch {
			// Background warm-up; Settings / Doctor / panel scan remain authoritative.
		}
	})();
}

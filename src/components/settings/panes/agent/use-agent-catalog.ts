import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
	catalogNeedsProbe,
	catalogProbeKey,
	customProbeKey,
	patchCatalogProbe,
	patchCustomProbe,
} from "@/components/settings/panes/agent-catalog";
import { useProbingKeys } from "@/components/settings/use-probing-keys";
import {
	type CatalogScanResponse,
	checkCatalogUpdates,
	openAgentLoginTerminal,
	probeAgent,
	probeCatalogAgent,
	scanCatalog,
} from "@/lib/agent";
import { errorText } from "@/lib/core/error";
import { notifyError } from "@/lib/core/notify";
import { hasHostTransport } from "@/lib/core/tauri";
import {
	remoteAgentProbe,
	remoteAgentScan,
} from "@/lib/vault/remote/remote-vault";

/** Where the agent catalog lives: this machine or the SSH host of a remote vault. */
export type AgentCatalogTransport = "local" | { remote: { sessionId: string } };

/**
 * Catalog scan + parallel ACP probe state shared by the local and remote agent
 * settings panes. `transport` selects the IPC backend; a remote scan response is
 * normalized into the local catalog shape (remote has no custom agents /
 * user-agent override).
 */
export function useAgentCatalog({
	transport,
}: {
	transport: AgentCatalogTransport;
}) {
	const { t } = useTranslation("settings");
	const [catalog, setCatalog] = useState<CatalogScanResponse | null>(null);
	const [loading, setLoading] = useState(false);
	const { probingKeys, setProbingKeys, clearProbingKey, clearAllProbingKeys } =
		useProbingKeys();
	const autoProbedRef = useRef(false);
	// Primitive extract — deps stay stable even when callers pass an inline
	// transport object (only the session id matters for the remote backend).
	const sessionId =
		typeof transport === "string" ? null : transport.remote.sessionId;

	/** Scan only — does not toggle busy; callers own the loading flag. */
	const scanOnce =
		useCallback(async (): Promise<CatalogScanResponse | null> => {
			if (!hasHostTransport()) {
				notifyError(t("agent.desktopOnly"));
				return null;
			}
			try {
				if (sessionId) {
					const scan = await remoteAgentScan(sessionId);
					const next: CatalogScanResponse = {
						entries: scan.entries,
						customAgents: [],
						defaultId: null,
						enabled: true,
						proxyEnabled: false,
						proxyUrl: "",
					};
					setCatalog(next);
					return next;
				}
				const scan = await scanCatalog();
				setCatalog(scan);
				return scan;
			} catch (e) {
				notifyError(errorText(e));
				return null;
			}
		}, [t, sessionId]);

	/**
	 * Local-only: re-scan and compare CLI versions so Upgrade shows only when a
	 * newer silent-update target is known. Remote vaults skip (no silent update).
	 */
	const refreshVersions =
		useCallback(async (): Promise<CatalogScanResponse | null> => {
			if (!hasHostTransport() || sessionId) return null;
			try {
				const scan = await checkCatalogUpdates();
				setCatalog(scan);
				return scan;
			} catch (e) {
				notifyError(errorText(e));
				return null;
			}
		}, [sessionId]);

	/**
	 * Parallel ACP probe. Soft open skips already-ready rows; force re-probes all
	 * installed. Badge updates from ProbeResult (no per-row full catalog rescan).
	 */
	const probeInstalled = useCallback(
		async (scan: CatalogScanResponse, force: boolean) => {
			if (!hasHostTransport()) return;
			const candidates = scan.entries.filter((e) =>
				catalogNeedsProbe(e, force),
			);
			// Remote scans carry no custom agents (customAgents is always [] there).
			const custom = scan.customAgents.filter(
				(a) => a.available && (force || a.lastProbeOk !== true),
			);
			if (candidates.length === 0 && custom.length === 0) {
				clearAllProbingKeys();
				return;
			}

			setProbingKeys(
				new Set([
					...candidates.map((e) => catalogProbeKey(e.templateId)),
					...custom.map((a) => customProbeKey(a.id)),
				]),
			);

			await Promise.allSettled([
				...candidates.map(async (entry) => {
					const key = catalogProbeKey(entry.templateId);
					try {
						const result = sessionId
							? await remoteAgentProbe(sessionId, entry.templateId)
							: await probeCatalogAgent(entry.templateId);
						setCatalog((prev) =>
							prev ? patchCatalogProbe(prev, entry.templateId, result) : prev,
						);
					} catch (e) {
						const err = errorText(e);
						setCatalog((prev) =>
							prev
								? patchCatalogProbe(prev, entry.templateId, {
										agentId: entry.registeredId ?? entry.templateId,
										available: false,
										error: err,
									})
								: prev,
						);
					} finally {
						clearProbingKey(key);
					}
				}),
				...custom.map(async (agent) => {
					const key = customProbeKey(agent.id);
					try {
						const result = await probeAgent(agent.id);
						setCatalog((prev) =>
							prev ? patchCustomProbe(prev, agent.id, result) : prev,
						);
					} catch (e) {
						const err = errorText(e);
						setCatalog((prev) =>
							prev
								? patchCustomProbe(prev, agent.id, {
										agentId: agent.id,
										available: false,
										error: err,
									})
								: prev,
						);
					} finally {
						clearProbingKey(key);
					}
				}),
			]);
		},
		[sessionId, clearProbingKey, clearAllProbingKeys, setProbingKeys],
	);

	/**
	 * PATH scan → parallel probe → one reconcile scan (local; remote keeps busy
	 * through probing and skips the reconcile re-scan over SSH).
	 * `force`: Refresh / proxy change re-probe everything; open page skips ready.
	 */
	const rescanAndProbe = useCallback(
		async (force = false) => {
			if (!hasHostTransport()) {
				notifyError(t("agent.desktopOnly"));
				return;
			}
			setLoading(true);
			try {
				if (sessionId) {
					const scan = await scanOnce();
					if (scan) await probeInstalled(scan, force);
					return;
				}
				const scan = await scanOnce();
				// Release global busy after PATH scan so Install stays clickable while
				// ACP probes (often multi-second / timeouts) run in the background.
				setLoading(false);
				if (scan) {
					await probeInstalled(scan, force);
					// Version check after probe so Upgrade reflects npm/pin targets.
					await refreshVersions();
				}
			} finally {
				setLoading(false);
				clearAllProbingKeys();
			}
		},
		[
			probeInstalled,
			refreshVersions,
			scanOnce,
			t,
			sessionId,
			clearAllProbingKeys,
		],
	);

	const refreshAfterLogin = useCallback(() => {
		const attempts = [2_000, 5_000, 10_000, 20_000];
		for (const delay of attempts) {
			window.setTimeout(() => {
				void rescanAndProbe(true);
			}, delay);
		}
	}, [rescanAndProbe]);

	const openLoginTerminal = useCallback(
		async (templateId: string): Promise<boolean> => {
			if (!hasHostTransport()) {
				notifyError(t("agent.desktopOnly"));
				return false;
			}
			if (sessionId) return false;
			try {
				await openAgentLoginTerminal(templateId);
				void refreshAfterLogin();
				return true;
			} catch (e) {
				notifyError(errorText(e));
				return false;
			}
		},
		[t, sessionId, refreshAfterLogin],
	);

	// Open once: soft probe (skip ready). Refresh / proxy use force=true.
	// Remote has no once-guard: soft probe when the session or callback changes.
	useEffect(() => {
		if (!sessionId) {
			if (autoProbedRef.current) return;
			autoProbedRef.current = true;
		}
		void rescanAndProbe(false);
	}, [rescanAndProbe, sessionId]);

	/** Mirror a committed ACP User-Agent into the catalog scan state. */
	const patchUserAgent = useCallback(
		(next: { userAgent: string; userAgentProviderIds: string }) => {
			setCatalog((prev) =>
				prev
					? {
							...prev,
							userAgent: next.userAgent,
							userAgentProviderIds: next.userAgentProviderIds,
						}
					: prev,
			);
		},
		[],
	);

	return {
		catalog,
		loading,
		setLoading,
		probingKeys,
		clearAllProbingKeys,
		scanOnce,
		probeInstalled,
		refreshVersions,
		rescanAndProbe,
		openLoginTerminal,
		patchUserAgent,
	};
}

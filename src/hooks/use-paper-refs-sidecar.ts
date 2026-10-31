import { useEffect, useRef, useState } from "react";
import { useTauriEvent } from "@/hooks/use-tauri-event";
import { events } from "@/lib/core/bindings";
import {
	CITE_SCHEMA_VERSION,
	type CiteSidecar,
	loadPaperRefsReadOnly,
} from "@/lib/paper/refs";

const normPaperPath = (p: string | null | undefined) =>
	(p ?? "").replace(/\\/g, "/").replace(/^\/+|\/+$/g, "");

/**
 * Read-only reference sidecar for a paper, reloaded when its ParseRefs
 * backfill job settles. Shared by the References panel and the PDF citation
 * hover preview (both need the same sidecar; JobCenter dedups the parse).
 */
export function usePaperRefsSidecar(
	vaultPath: string | null,
	paperPath: string | null,
): {
	sidecar: CiteSidecar | null;
	loading: boolean;
	/**
	 * A stale (older-schema) sidecar is shown while its background re-parse
	 * runs; panels can show an "enriching" hint instead of a flash of placeholders.
	 */
	upgrading: boolean;
	setSidecar: (sidecar: CiteSidecar | null) => void;
} {
	const [sidecar, setSidecar] = useState<CiteSidecar | null>(null);
	const [loading, setLoading] = useState(false);
	const [upgrading, setUpgrading] = useState(false);
	const reloadRef = useRef<(() => void) | null>(null);

	// Reload when this paper's ParseRefs backfill settles (event-driven,
	// replacing the old blocking list→parse fallback).
	useTauriEvent(events.jobChanged, ({ job }) => {
		if (job.kind !== "parseRefs") return;
		if (normPaperPath(job.paperPath) !== normPaperPath(paperPath)) return;
		if (
			job.state === "succeeded" ||
			job.state === "failed" ||
			job.state === "cancelled"
		) {
			reloadRef.current?.();
		}
	});

	useEffect(() => {
		setSidecar(null);
		setUpgrading(false);
		if (!vaultPath || !paperPath) {
			reloadRef.current = null;
			return;
		}
		let cancelled = false;
		setLoading(true);
		const reload = () => {
			loadPaperRefsReadOnly(vaultPath, paperPath)
				.then((s) => {
					if (cancelled) return;
					setSidecar(s);
					setUpgrading(Boolean(s && s.schemaVersion < CITE_SCHEMA_VERSION));
				})
				.catch(() => {
					if (!cancelled) setSidecar(null);
				})
				.finally(() => {
					if (!cancelled) setLoading(false);
				});
		};
		reload();
		reloadRef.current = reload;
		return () => {
			cancelled = true;
			reloadRef.current = null;
		};
	}, [vaultPath, paperPath]);

	return { sidecar, loading, upgrading, setSidecar };
}

import { commands, type UsageRecord } from "@/lib/core/bindings";
import { callApi, callApiResult } from "@/lib/core/ipc";
import { hasHostTransport } from "@/lib/core/tauri";

/** Wire shapes from the generated bindings (usage.sqlite rows). */
export type ActivityRecord = UsageRecord;

export async function recordActivityEvents(
	events: ActivityRecord[],
): Promise<number> {
	if (!hasHostTransport() || events.length === 0) return 0;
	return callApiResult(() => commands.activityRecordEvents({ events }), {
		fallback: "activity_record_events failed",
	});
}

export async function clearUsage(vault?: string): Promise<number> {
	if (!hasHostTransport()) return 0;
	return callApi(() => commands.usageClear({ vault: vault ?? null }), {
		fallback: "usage_clear failed",
	});
}

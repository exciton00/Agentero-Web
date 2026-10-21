import { commands } from "@/lib/core/bindings";
import { hasHostTransport } from "@/lib/core/tauri";

/** Host marker for "the built-in provider has no compiled-in key". */
export const ERR_TRANSLATE_NO_BUILTIN_KEY = "translate.no_builtin_key";

/**
 * Host MT command via the generated typed binding (tauri-specta pilot).
 * `provider` selects a free web engine or a commercial BYOK engine.
 * Regenerate bindings: `cargo test -p agentero export_typescript_bindings`.
 */
export async function invokeTranslateText(args: {
	text: string;
	sourceLang: string;
	targetLang: string;
	provider: string;
	apiKey?: string;
	baseUrl?: string;
	region?: string;
	model?: string;
	/** Host request timeout (ms); clamped 1s–30s server-side. */
	timeoutMs?: number;
}): Promise<string> {
	if (!hasHostTransport()) {
		throw new Error("Free translation requires the Tauri desktop app.");
	}
	const res = await commands.translateText({
		text: args.text,
		sourceLang: args.sourceLang,
		targetLang: args.targetLang,
		provider: args.provider,
		apiKey: args.apiKey?.trim() || null,
		baseUrl: args.baseUrl?.trim() || null,
		region: args.region?.trim() || null,
		model: args.model?.trim() || null,
		timeoutMs: args.timeoutMs ?? null,
	});
	if (!res.ok || !res.data) {
		// Only a known domain marker is surfaced verbatim for `displayTranslateError`
		// to map. `AppError::code()` also yields generic codes (`io`, `json`,
		// `sqlite`), so treating "not `message`" as a marker would eventually show
		// users a bare code instead of the Host's human text.
		const code = res.error?.code;
		throw new Error(
			code === ERR_TRANSLATE_NO_BUILTIN_KEY
				? code
				: (res.error?.message ?? "translate failed"),
		);
	}
	return res.data.text;
}

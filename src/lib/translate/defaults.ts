import type {
	TranslateProviderConfig,
	TranslateSettings,
} from "@/lib/translate/types";

/** Full-document translation font multipliers; mirrors the Rust clamp. */
export const TRANSLATE_FONT_SCALE_MIN = 0.5;
export const TRANSLATE_FONT_SCALE_MAX = 2;
export const TRANSLATE_FONT_SCALE_DEFAULT = 1;

/** Clamp a persisted font-scale preference (non-finite → default). */
export function clampTranslateFontScale(value: unknown): number {
	if (typeof value !== "number" || !Number.isFinite(value)) {
		return TRANSLATE_FONT_SCALE_DEFAULT;
	}
	return Math.min(
		TRANSLATE_FONT_SCALE_MAX,
		Math.max(TRANSLATE_FONT_SCALE_MIN, value),
	);
}

export const DEFAULT_TRANSLATE_SETTINGS: TranslateSettings = {
	/** Prefer Tencent Transmart: current no-key default with better availability. */
	provider: "tencenttransmart",
	targetLang: "ui",
	sourceLang: "auto",
	providerConfigs: {},
	autoTranslateSelection: false,
	dualPaneTranslate: false,
	displayMode: "overlay",
	dualPaneSource: "pdf",
	bodyFontScale: TRANSLATE_FONT_SCALE_DEFAULT,
	headingFontScale: TRANSLATE_FONT_SCALE_DEFAULT,
	agentId: "",
	modelId: "",
	customPrompt: "",
	customSystem: "",
	customUser: "",
};

/** Blank commercial provider config (missing draft/stored entry fallback). */
export const EMPTY_TRANSLATE_PROVIDER_CONFIG: TranslateProviderConfig = {
	apiKey: "",
	baseUrl: "",
	region: "",
	model: "",
};

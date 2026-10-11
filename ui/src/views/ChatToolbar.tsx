import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useCloseOnOutside } from "../components/Dropdown";
import {
  DEFAULT_CONTEXT_LENGTH,
  DEFAULT_SAMPLING,
  recommendedSampling,
  SAMPLING_PARAM_KEYS,
  samplingParamsEqual,
  type SamplingParamKey,
  type SamplingParams,
} from "../lib/types";
import { useStore } from "../state/store";

const SlidersIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
    strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <line x1="4" y1="6" x2="20" y2="6" />
    <circle cx="9" cy="6" r="2" fill="currentColor" stroke="none" />
    <line x1="4" y1="12" x2="20" y2="12" />
    <circle cx="15" cy="12" r="2" fill="currentColor" stroke="none" />
    <line x1="4" y1="18" x2="20" y2="18" />
    <circle cx="11" cy="18" r="2" fill="currentColor" stroke="none" />
  </svg>
);

const CONTEXT_LENGTH_OPTIONS = [2048, 4096, 8192, 16384, 32768, 65536, 131072];

/** A labeled range slider showing its own current value, alongside the
 * flanking min/max hint labels every slider in this panel already used. */
function SliderField({
  label,
  hint,
  min,
  max,
  step,
  decimals,
  value,
  onChange,
  leftLabel,
  rightLabel,
}: {
  label: string;
  hint: string;
  min: number;
  max: number;
  step: number;
  /** Decimal places to show in the value readout (0 for integer knobs). */
  decimals: number;
  value: number;
  onChange: (n: number) => void;
  leftLabel: string;
  rightLabel: string;
}) {
  return (
    <label className="settings-field">
      <div className="settings-label-row">
        <span className="settings-label">{label}</span>
        <span className="settings-value">{value.toFixed(decimals)}</span>
      </div>
      <span className="settings-hint">{hint}</span>
      <div className="settings-slider">
        <span>{leftLabel}</span>
        <input
          type="range"
          min={min}
          max={max}
          step={step}
          value={value}
          onChange={(e) => onChange(Number(e.target.value))}
        />
        <span>{rightLabel}</span>
      </div>
    </label>
  );
}

/**
 * Per-conversation generation tuning: llama.cpp's standard sampling knobs
 * plus context length. Every change applies to the active conversation at
 * once (used by the very next message) via `updateGeneration`, which also
 * handles persistence — this component only reads current values and
 * dispatches patches. Lives inside `ChatToolbar`'s panel, which owns the
 * open/close behavior.
 */
function GenerationPanel() {
  const { t } = useTranslation();
  const active = useStore((s) => s.active);
  const models = useStore((s) => s.models);
  const cloudModels = useStore((s) => s.cloudModels);
  const settings = useStore((s) => s.settings);
  const engine = useStore((s) => s.engine);
  const engineBusy = useStore((s) => s.engineBusy);
  const runningContextLength = useStore((s) => s.runningContextLength);
  const useModel = useStore((s) => s.useModel);
  const updateGeneration = useStore((s) => s.updateGeneration);

  const sampling = active?.sampling ?? DEFAULT_SAMPLING;
  const contextLength = active?.contextLength ?? DEFAULT_CONTEXT_LENGTH;
  const patchSampling = (fields: Partial<SamplingParams>) =>
    updateGeneration({ sampling: { ...sampling, ...fields } });

  // A cloud model has no local llama-server process for this app to launch
  // with a `-c` flag — its context window is a fixed, shared setting on the
  // backend, not something this conversation can change or "restart" into.
  const cloudMode = settings?.chatBackend === "cloud";

  // Local llama.cpp honors every SamplingParams field uniformly, so a local
  // chat always gets the full panel. A cloud model's underlying provider
  // varies sharply (OpenAI has no top_k/min_p/repeat_penalty/repeat_last_n
  // at all; Anthropic additionally drops the penalties and seed) — gate on
  // the selected model's own declared `supportedParams`, the same way a
  // multi-provider gateway like OpenRouter or LiteLLM only forwards a field
  // to a provider that's declared it'll honor, rather than guessing or
  // showing a control that would silently do nothing (or get the whole
  // request rejected by a strict backend). Nothing selected yet (or the
  // catalog hasn't loaded) shows no sliders rather than a guess.
  const selectedCloudModel = cloudModels?.find((m) => m.id === settings?.selectedCloudModelId);
  const supportedParams = new Set<SamplingParamKey>(
    cloudMode ? (selectedCloudModel?.supportedParams ?? []) : SAMPLING_PARAM_KEYS,
  );
  const show = (key: SamplingParamKey) => supportedParams.has(key);

  const selectedSpec = models.find((m) => m.spec.id === settings?.selectedModelId)?.spec;
  const contextOptions = CONTEXT_LENGTH_OPTIONS.filter(
    (n) => !selectedSpec || n <= selectedSpec.contextMax,
  );
  const runningModelId = engine.type === "running" ? engine.data.modelId : null;
  const contextNeedsRestart =
    runningModelId !== null && runningContextLength !== null && runningContextLength !== contextLength;

  // `ModelSpec.taskDefaults` (and so a per-model recommendation) only exists
  // for the local curated catalog — a cloud model always gets the general
  // default, same gate `store.ts`'s `newChat` uses. Uses the conversation's
  // live `taskType` (set silently by `useTriageAutopilot`, `ChatView.tsx`)
  // rather than hardcoding "general", so this hint/reset stays correct after
  // a request gets auto-classified as e.g. coding.
  const recommended = recommendedSampling(
    cloudMode ? undefined : selectedSpec,
    active?.taskType ?? "general",
    settings?.showThinking ?? true,
  );
  const isAutoSuggested = !cloudMode && samplingParamsEqual(sampling, recommended);
  const reset = () =>
    updateGeneration(
      cloudMode ? { sampling: recommended } : { sampling: recommended, contextLength: DEFAULT_CONTEXT_LENGTH },
    );
  const hasAdvancedContent =
    !cloudMode || show("repeatLastN") || show("frequencyPenalty") || show("maxTokens") || show("seed");

  return (
    <div className="toolbar-generation">
      <div className="gen-panel-header">
        <h3>{t("chatToolbar.generation.title")}</h3>
        <button type="button" className="btn-quiet" onClick={reset}>
          {t("chatToolbar.generation.resetToDefaults")}
        </button>
      </div>
      <p className="settings-hint">
        {t("chatToolbar.generation.hint")}
        {cloudMode && t("chatToolbar.generation.cloudContextHint")}
      </p>
      {isAutoSuggested && selectedSpec && (
        <p className="settings-hint gen-auto-suggested">
          {t("chatToolbar.generation.autoSuggested", { model: selectedSpec.name })}
        </p>
      )}

      {cloudMode && supportedParams.size === 0 && (
        <p className="settings-hint">{t("chatToolbar.cloudNoParamsHint")}</p>
      )}

      {show("temperature") && (
        <SliderField
          label={t("chatToolbar.sliders.temperature.label")}
          hint={t("chatToolbar.sliders.temperature.hint")}
          min={0}
          max={1.4}
          step={0.1}
          decimals={1}
          value={sampling.temperature}
          onChange={(n) => patchSampling({ temperature: n })}
          leftLabel={t("chatToolbar.sliders.temperature.left")}
          rightLabel={t("chatToolbar.sliders.temperature.right")}
        />
      )}

      {show("topP") && (
        <SliderField
          label={t("chatToolbar.sliders.topP.label")}
          hint={t("chatToolbar.sliders.topP.hint")}
          min={0}
          max={1}
          step={0.01}
          decimals={2}
          value={sampling.topP}
          onChange={(n) => patchSampling({ topP: n })}
          leftLabel={t("chatToolbar.sliders.topP.left")}
          rightLabel={t("chatToolbar.sliders.topP.right")}
        />
      )}

      {show("topK") && (
        <SliderField
          label={t("chatToolbar.sliders.topK.label")}
          hint={t("chatToolbar.sliders.topK.hint")}
          min={0}
          max={100}
          step={1}
          decimals={0}
          value={sampling.topK}
          onChange={(n) => patchSampling({ topK: n })}
          leftLabel={t("chatToolbar.sliders.topK.left")}
          rightLabel={t("chatToolbar.sliders.topK.right")}
        />
      )}

      {show("minP") && (
        <SliderField
          label={t("chatToolbar.sliders.minP.label")}
          hint={t("chatToolbar.sliders.minP.hint")}
          min={0}
          max={0.5}
          step={0.01}
          decimals={2}
          value={sampling.minP}
          onChange={(n) => patchSampling({ minP: n })}
          leftLabel={t("chatToolbar.sliders.minP.left")}
          rightLabel={t("chatToolbar.sliders.minP.right")}
        />
      )}

      {show("presencePenalty") && (
        <SliderField
          label={t("chatToolbar.sliders.presencePenalty.label")}
          hint={t("chatToolbar.sliders.presencePenalty.hint")}
          min={-2}
          max={2}
          step={0.1}
          decimals={1}
          value={sampling.presencePenalty}
          onChange={(n) => patchSampling({ presencePenalty: n })}
          leftLabel={t("chatToolbar.sliders.presencePenalty.left")}
          rightLabel={t("chatToolbar.sliders.presencePenalty.right")}
        />
      )}

      {show("repeatPenalty") && (
        <SliderField
          label={t("chatToolbar.sliders.repeatPenalty.label")}
          hint={t("chatToolbar.sliders.repeatPenalty.hint")}
          min={1}
          max={2}
          step={0.01}
          decimals={2}
          value={sampling.repeatPenalty}
          onChange={(n) => patchSampling({ repeatPenalty: n })}
          leftLabel={t("chatToolbar.sliders.repeatPenalty.left")}
          rightLabel={t("chatToolbar.sliders.repeatPenalty.right")}
        />
      )}

      {hasAdvancedContent && (
        <details className="settings-advanced">
          <summary>{t("chatToolbar.advanced.summary")}</summary>

          {!cloudMode && (
            <div className="settings-field">
              <span className="settings-label">{t("chatToolbar.contextLength.label")}</span>
              <span className="settings-hint">{t("chatToolbar.contextLength.hint")}</span>
              <select
                className="settings-select"
                value={contextLength}
                onChange={(e) => updateGeneration({ contextLength: Number(e.target.value) })}
              >
                {contextOptions.map((n) => (
                  <option key={n} value={n}>
                    {t("chatToolbar.contextLength.tokensOption", { count: n, value: n.toLocaleString() })}
                  </option>
                ))}
              </select>
              {contextNeedsRestart && (
                <div className="settings-restart-row">
                  <span className="settings-hint">{t("chatToolbar.contextLength.restartHint")}</span>
                  <button
                    className="btn-quiet"
                    disabled={engineBusy}
                    onClick={() => runningModelId && void useModel(runningModelId, contextLength)}
                  >
                    {t("chatToolbar.contextLength.restartButton")}
                  </button>
                </div>
              )}
            </div>
          )}

          {show("repeatLastN") && (
            <SliderField
              label={t("chatToolbar.sliders.repeatLastN.label")}
              hint={t("chatToolbar.sliders.repeatLastN.hint")}
              min={0}
              max={256}
              step={8}
              decimals={0}
              value={sampling.repeatLastN}
              onChange={(n) => patchSampling({ repeatLastN: n })}
              leftLabel={t("chatToolbar.sliders.repeatLastN.left")}
              rightLabel={t("chatToolbar.sliders.repeatLastN.right")}
            />
          )}

          {show("frequencyPenalty") && (
            <SliderField
              label={t("chatToolbar.sliders.frequencyPenalty.label")}
              hint={t("chatToolbar.sliders.frequencyPenalty.hint")}
              min={-2}
              max={2}
              step={0.1}
              decimals={1}
              value={sampling.frequencyPenalty}
              onChange={(n) => patchSampling({ frequencyPenalty: n })}
              leftLabel={t("chatToolbar.sliders.frequencyPenalty.left")}
              rightLabel={t("chatToolbar.sliders.frequencyPenalty.right")}
            />
          )}

          {show("maxTokens") && (
            <div className="settings-field">
              <span className="settings-label">{t("chatToolbar.maxTokens.label")}</span>
              <span className="settings-hint">{t("chatToolbar.maxTokens.hint")}</span>
              <div className="settings-sentinel-row">
                <label className="settings-toggle">
                  <input
                    type="checkbox"
                    checked={sampling.maxTokens < 0}
                    onChange={(e) =>
                      patchSampling({ maxTokens: e.target.checked ? -1 : 512 })
                    }
                  />
                  <span>{t("chatToolbar.maxTokens.noLimit")}</span>
                </label>
                <input
                  type="number"
                  className="settings-number"
                  min={1}
                  step={16}
                  value={sampling.maxTokens < 0 ? 512 : sampling.maxTokens}
                  disabled={sampling.maxTokens < 0}
                  onChange={(e) => patchSampling({ maxTokens: Number(e.target.value) })}
                />
              </div>
            </div>
          )}

          {show("seed") && (
            <div className="settings-field">
              <span className="settings-label">{t("chatToolbar.seed.label")}</span>
              <span className="settings-hint">{t("chatToolbar.seed.hint")}</span>
              <div className="settings-sentinel-row">
                <label className="settings-toggle">
                  <input
                    type="checkbox"
                    checked={sampling.seed < 0}
                    onChange={(e) => patchSampling({ seed: e.target.checked ? -1 : 0 })}
                  />
                  <span>{t("chatToolbar.seed.random")}</span>
                </label>
                <input
                  type="number"
                  className="settings-number"
                  min={0}
                  step={1}
                  value={sampling.seed < 0 ? 0 : sampling.seed}
                  disabled={sampling.seed < 0}
                  onChange={(e) => patchSampling({ seed: Number(e.target.value) })}
                />
              </div>
            </div>
          )}
        </details>
      )}
    </div>
  );
}

/**
 * Top-right conversation toolbar: a stats readout (message count, context
 * length) above the per-conversation Generation panel. Replaces the
 * composer's old tune button — this lives in the app's title bar instead so
 * it's reachable without cluttering the chat input row.
 */
export default function ChatToolbar() {
  const { t } = useTranslation();
  const active = useStore((s) => s.active);
  const [open, setOpen] = useState(false);
  const panelRef = useCloseOnOutside<HTMLDivElement>(open, () => setOpen(false));
  const messageCount = active?.messages.length ?? 0;
  const contextLength = active?.contextLength ?? DEFAULT_CONTEXT_LENGTH;

  return (
    <div className="menu-wrap">
      <button
        type="button"
        className={`icon-btn${open ? " is-on" : ""}`}
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="dialog"
        aria-expanded={open}
        title={t("chatToolbar.conversationSettings")}
        aria-label={t("chatToolbar.conversationSettings")}
      >
        <SlidersIcon />
      </button>
      {open && (
        <div
          className="toolbar-panel"
          ref={panelRef}
          role="dialog"
          aria-label={t("chatToolbar.conversationSettings")}
        >
          <div className="toolbar-stats">
            <div className="toolbar-stat">
              <span className="toolbar-stat-value">{messageCount}</span>
              <span className="toolbar-stat-label">
                {t("chatToolbar.stats.messages", { count: messageCount })}
              </span>
            </div>
            <div className="toolbar-stat">
              <span className="toolbar-stat-value">{contextLength.toLocaleString()}</span>
              <span className="toolbar-stat-label">{t("chatToolbar.stats.contextTokens")}</span>
            </div>
          </div>
          <GenerationPanel />
        </div>
      )}
    </div>
  );
}

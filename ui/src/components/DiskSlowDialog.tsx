import { useEffect, useId, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { api } from "../lib/api";
import { formatSpeed, formatTokensPerSec } from "../lib/format";
import type { DiskSpeedInfo, ModelInfo } from "../lib/types";
import { useStore } from "../state/store";
import { useDialogFocus } from "./useDialogFocus";

/** While the dialog is open, either we're still checking, the check itself
 * failed (fails open — the generic copy still lets the user proceed), or we
 * have a real measurement. */
type SpeedProbe = "checking" | "unknown" | DiskSpeedInfo;

/** A "tooSlow" measurement is a hard refusal — `docs/design/DISK_OFFLOAD.md` Phase
 * 3's "Refuse (keep TooBig) below a floor" design decision — so it never gets
 * a "download anyway" escape hatch. */
function isBlockedBySpeed(probe: SpeedProbe): boolean {
  return typeof probe === "object" && probe.class === "tooSlow";
}

/** The dialog's message: honest once we've measured the disk, generic while
 * checking or if the check itself failed. Takes `t` explicitly (rather than
 * calling the `useTranslation` hook itself) since this is a plain helper, not
 * a component — its caller already holds one. */
function diskSlowConfirmCopy(
  probe: SpeedProbe,
  tFull: ReturnType<typeof useTranslation>["t"],
): string {
  // Cast once, here, to a plain, cheap function type — every call below
  // passes a literal key (optionally with a plain interpolation object) and
  // nothing i18next-specific beyond that, so nothing real is lost. Calling
  // `tFull` directly (its real, heavily overloaded i18next type) repeatedly
  // hits TS's "type instantiation is excessively deep" limit as
  // CustomTypeOptions' resources grow (see `i18n/index.ts`'s comment on the
  // `recovery`/`remoteAccess` namespaces) — resolving the full overload set
  // per call is far more work than this helper needs. Mirrors
  // `ToolConsentCard.tsx`'s `dangerFlagText`, which hit the same limit.
  const t = tFull as (key: string, options?: Record<string, unknown>) => string;
  const base = t("library.diskSlowConfirm.base");
  if (probe === "checking") return `${base} ${t("library.diskSlowConfirm.checking")}`;
  if (probe === "unknown") return `${base} ${t("library.diskSlowConfirm.stillUnknown")}`;
  if (isBlockedBySpeed(probe)) {
    return `${base} ${t("library.diskSlowConfirm.blocked")}`;
  }
  const speed = t("library.diskSlowConfirm.speedEstimate", {
    speed: formatSpeed(probe.bytesPerSec),
  });
  const tokens =
    probe.estimatedTokensPerSec != null
      ? `, ${formatTokensPerSec(probe.estimatedTokensPerSec)}.`
      : `. ${t("library.diskSlowConfirm.stillUnknown")}`;
  return `${base} ${speed}${tokens}`;
}

/**
 * The "yes, this will be slow" consent step before a fresh download of a
 * disk-streamed model. A modal rather than `ConfirmDialog`, because it needs
 * three things that generic dialog deliberately doesn't offer: message text
 * that updates live (and must announce) as the disk probe resolves, an
 * optional "there's a faster model" tip, and a confirm button that has to
 * disappear entirely — not just disable — when the disk measures too slow to
 * be usable at all.
 *
 * Owns the probe itself: it only exists while open, so the measurement runs
 * on mount instead of being gated on a boolean in a parent's hook. Cheap to
 * re-run, and the drive (portable build) or its condition could have changed
 * since last time.
 */
export default function DiskSlowDialog({
  model,
  onConfirm,
  onCancel,
}: {
  model: ModelInfo;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const titleId = useId();
  const dialogRef = useDialogFocus<HTMLDivElement>(onCancel);
  const [probe, setProbe] = useState<SpeedProbe>("checking");
  const allModels = useStore((s) => s.models);
  const { spec } = model;

  useEffect(() => {
    let cancelled = false;
    setProbe("checking");
    api.probeDiskSpeed(spec.id).then(
      (info) => {
        if (!cancelled) setProbe(info);
      },
      () => {
        if (!cancelled) setProbe("unknown");
      },
    );
    return () => {
      cancelled = true;
    };
  }, [spec.id]);

  // The biggest lever for "smart but usable" is picking a model that fits in
  // memory instead — the real-hardware bench (docs/design/DISK_OFFLOAD.md) measured
  // a 30-140x speed gap between disk-streamed and comfortably-fitting MoE
  // models on the same machine. Surface the best-fitting alternative right
  // here, where the user is deciding.
  const fastAlternative = allModels
    .filter((m) => m.spec.id !== spec.id && (m.fit === "perfect" || m.fit === "good"))
    .reduce<ModelInfo | null>(
      (best, m) => (best == null || m.spec.qualityScore > best.spec.qualityScore ? m : best),
      null,
    );

  const blocked = isBlockedBySpeed(probe);

  return createPortal(
    // Click-to-dismiss is tested on the backdrop itself rather than being
    // cancelled by a `stopPropagation` handler on the dialog, so neither
    // element needs a click handler that a keyboard can't reach — Escape,
    // via `useDialogFocus`, is the keyboard equivalent. Same behavior as
    // `ConfirmDialog`, expressed without the static-element handlers.
    <div
      className="modal-backdrop"
      role="presentation"
      onClick={(e) => {
        if (e.target === e.currentTarget) onCancel();
      }}
    >
      <div
        ref={dialogRef}
        tabIndex={-1}
        className="modal"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={titleId}
      >
        <h2 id={titleId}>{t("library.diskSlowConfirm.title", { name: spec.name })}</h2>
        <div className="model-confirm">
          {/* Text changes as the probe resolves from "checking" to a real
              measurement — announce that update. */}
          <p className="model-confirm-text" role="status">
            {diskSlowConfirmCopy(probe, t)}
          </p>
          {fastAlternative && (
            <p className="model-confirm-tip">
              {t("library.diskSlowConfirm.fasterAlternative", {
                name: fastAlternative.spec.name,
              })}
            </p>
          )}
          <div className="model-confirm-actions">
            <button className="btn-quiet" onClick={onCancel}>
              {t("common.cancel")}
            </button>
            {!blocked && (
              <button
                className="btn-ghost"
                disabled={probe === "checking"}
                onClick={onConfirm}
              >
                {t("library.downloadAnyway")}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>,
    document.body,
  );
}

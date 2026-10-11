import { useTranslation } from "react-i18next";
import i18n from "../i18n";
import ProgressBar from "../components/ProgressBar";
import { usePluginDisplayName } from "../lib/pluginName";
import type { PluginInfo, ServiceState } from "../lib/types";
import { useStore } from "../state/store";

/** A small status badge for a plugin's running service, or null when not shown.
 * Calls `i18n.t()` directly rather than the `useTranslation()` hook since this
 * is a plain helper, not a component — see format.ts's `fitLabel` for the same
 * pattern. */
function serviceBadge(service: ServiceState) {
  switch (service.type) {
    case "running":
      return { label: i18n.t("plugins.service.running"), className: "badge-perfect" };
    case "starting":
      return { label: i18n.t("plugins.service.starting"), className: "badge-tight" };
    case "stopped":
      return { label: i18n.t("plugins.service.stopped"), className: "badge-neutral" };
    case "failed":
      return { label: i18n.t("plugins.service.failed"), className: "badge-danger" };
    case "notApplicable":
      return null;
  }
}

export default function Plugins() {
  const { t } = useTranslation();
  const plugins = useStore((s) => s.plugins);

  return (
    <div className="plugins">
      <div className="plugins-scroll">
        <div className="plugins-col">
          <header className="plugins-head">
            <h1>{t("plugins.title")}</h1>
            <p>{t("plugins.subtitle")}</p>
          </header>

          {plugins.length === 0 ? (
            <div className="plugins-empty">
              <p>{t("plugins.empty")}</p>
            </div>
          ) : (
            <div className="plugin-grid">
              {plugins.map((p) => (
                <PluginCard key={p.id} plugin={p} />
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function PluginCard({ plugin }: { plugin: PluginInfo }) {
  const { t } = useTranslation();
  const setPluginEnabled = useStore((s) => s.setPluginEnabled);
  const name = usePluginDisplayName()(plugin);

  const { id, description, version, author, capabilities, enabled, setup, service, experimental } =
    plugin;

  const settingUp = setup.type === "settingUp";
  const setupFailed = setup.type === "failed";
  const serviceFailed = service.type === "failed";
  const badge = serviceBadge(service);

  const toggle = () => {
    void setPluginEnabled(id, !enabled);
  };
  const retry = () => {
    void setPluginEnabled(id, true);
  };

  return (
    <article className="plugin-card">
      <div className="plugin-card-head">
        <h2>{name}</h2>
        <label
          className="plugin-switch"
          title={enabled ? t("plugins.disable") : t("plugins.enable")}
        >
          <input
            type="checkbox"
            aria-label={enabled ? t("plugins.disable") : t("plugins.enable")}
            checked={enabled}
            disabled={settingUp}
            onChange={toggle}
          />
          <span className="plugin-switch-track" aria-hidden="true">
            <span className="plugin-switch-thumb" />
          </span>
        </label>
      </div>

      <p className="plugin-meta">{t("plugins.versionByAuthor", { version, author })}</p>
      <p className="plugin-desc">{description}</p>

      {(experimental || capabilities.length > 0 || badge) && (
        <div className="plugin-chips">
          {experimental && (
            <span className="badge badge-accent">{t("plugins.experimental")}</span>
          )}
          {capabilities.map((c) => (
            <span key={c} className="badge badge-neutral">
              {c}
            </span>
          ))}
          {badge && <span className={`badge ${badge.className}`}>{badge.label}</span>}
        </div>
      )}

      {settingUp && (
        <div className="plugin-setup">
          <ProgressBar
            className={`plugin-progress${setup.data.fraction === null ? " is-indeterminate" : ""}`}
            barClassName="plugin-progress-bar"
            value={setup.data.fraction}
            label={t("plugins.setupProgressAriaLabel", { name })}
          />
          <span className="plugin-setup-msg">{setup.data.message}</span>
        </div>
      )}

      {setupFailed && (
        <div className="plugin-failure">
          <p className="plugin-error" role="alert">{setup.data.message}</p>
          <button className="btn-ghost" onClick={retry}>
            {t("plugins.retry")}
          </button>
        </div>
      )}

      {!setupFailed && serviceFailed && (
        <div className="plugin-failure">
          <p className="plugin-error" role="alert">{service.data.message}</p>
          <button className="btn-ghost" onClick={retry}>
            {t("plugins.retry")}
          </button>
        </div>
      )}
    </article>
  );
}

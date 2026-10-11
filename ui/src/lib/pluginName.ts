import { useTranslation } from "react-i18next";
import type { PluginInfo } from "./types";

/** A plugin's name as the user should see it. Names come from each
 * manifest's `name` (`plugin.json`), which is English-only; a built-in
 * plugin with a translated name is mapped to its i18n key here instead. */
export function usePluginDisplayName(): (plugin: Pick<PluginInfo, "id" | "name">) => string {
  const { t } = useTranslation();
  return (plugin) =>
    plugin.id === "smart-model-router" ? t("plugins.names.smartModelRouter") : plugin.name;
}

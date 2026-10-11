import { useTranslation } from "react-i18next";
import { useStore } from "../state/store";

const StarIcon = ({ filled }: { filled: boolean }) => (
  <svg
    viewBox="0 0 24 24"
    fill={filled ? "currentColor" : "none"}
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M12 2.7l2.9 6 6.6.9-4.8 4.6 1.1 6.6-5.8-3.2-5.8 3.2 1.1-6.6-4.8-4.6 6.6-.9z" />
  </svg>
);

export interface FavoriteButtonProps {
  modelId: string;
  /** The model's display name, folded into the accessible label. */
  name: string;
  favorite: boolean;
}

/**
 * Icon-only star toggle for marking a model as a favorite — shown on the
 * Library page (`ModelCard` and `ModelRow`) and the composer's model
 * quick-switcher (`ModelQuickPicker`), so every place gets the exact same
 * look and store wiring instead of three hand-rolled copies.
 */
export default function FavoriteButton({ modelId, name, favorite }: FavoriteButtonProps) {
  const { t } = useTranslation();
  const setModelFavorite = useStore((s) => s.setModelFavorite);
  const label = t(favorite ? "common.unfavorite" : "common.favorite", { name });

  return (
    <button
      type="button"
      className={`favorite-btn${favorite ? " is-favorite" : ""}`}
      aria-pressed={favorite}
      aria-label={label}
      title={label}
      onClick={(e) => {
        // Every call site nests this inside a larger clickable row (a model
        // card, a table row, the quick-picker's model option) — never let the
        // star's own click also trigger that row's action.
        e.stopPropagation();
        void setModelFavorite(modelId, !favorite);
      }}
    >
      <StarIcon filled={favorite} />
    </button>
  );
}

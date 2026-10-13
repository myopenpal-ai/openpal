import { useId } from "react";
import type { Theme } from "../lib/types";

export interface ThemePickerOption {
  value: Theme;
  label: string;
  /** A second line under the label (onboarding's "Matches your device" under
   * System). It describes the option rather than naming it. */
  hint?: string;
}

interface ThemePickerProps {
  value: Theme;
  onChange: (theme: Theme) => void;
  /** Rendered in this order. Text comes from the caller: Settings and the
   * onboarding step each have their own copy keys. */
  options: ThemePickerOption[];
  /** The id of the element that names the group: the visible "Theme" label in
   * Settings, the scene heading in onboarding. */
  labelledBy: string;
  /** The id of the element that describes the group (a hint, a sub line), if
   * there is one. */
  describedBy?: string;
  /** Only picks the look — `theme-picker.css`: a segmented rail for Settings,
   * three cards for onboarding. */
  variant: "segmented" | "cards";
}

/**
 * The System / Light / Dark choice, shared by Settings → General and the
 * onboarding step.
 *
 * Native radio inputs inside a labelled `radiogroup`, so the platform supplies
 * the whole keyboard model (one Tab stop on the checked option, arrows move and
 * select) and a screen reader hears the group's name, each option's name and
 * which one is checked. Each radio is named by its own label text only
 * (`aria-labelledby`) so an option's hint is announced as its description, not
 * as part of its name.
 *
 * The inputs are invisible overlays over the whole option, so the option stays
 * focusable and fully clickable; what you see is the sibling `.theme-option-face`.
 * Selection is `:checked` on the real input, never a class, so what is painted
 * can't drift from what is announced.
 *
 * Controlled by the caller: `onChange` is expected to update `value` in the same
 * tick (the store's `setTheme` does), which is what keeps a controlled radio from
 * snapping back for a frame.
 */
export default function ThemePicker({
  value,
  onChange,
  options,
  labelledBy,
  describedBy,
  variant,
}: ThemePickerProps) {
  const id = useId();
  return (
    <div
      role="radiogroup"
      aria-labelledby={labelledBy}
      aria-describedby={describedBy}
      className={`theme-picker theme-picker-${variant}`}
    >
      {options.map((option) => {
        const inputId = `${id}-${option.value}`;
        const labelId = `${inputId}-label`;
        const hintId = option.hint ? `${inputId}-hint` : undefined;
        return (
          <label key={option.value} htmlFor={inputId} className="theme-option">
            <input
              id={inputId}
              type="radio"
              className="theme-option-input"
              name={id}
              value={option.value}
              checked={option.value === value}
              onChange={() => onChange(option.value)}
              aria-labelledby={labelId}
              aria-describedby={hintId}
            />
            <span className="theme-option-face">
              <span className="theme-option-mark" aria-hidden="true" />
              <span className="theme-option-text">
                <span id={labelId} className="theme-option-label">
                  {option.label}
                </span>
                {option.hint && (
                  <span id={hintId} className="theme-option-hint">
                    {option.hint}
                  </span>
                )}
              </span>
            </span>
          </label>
        );
      })}
    </div>
  );
}

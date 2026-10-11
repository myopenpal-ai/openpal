import tsParser from "@typescript-eslint/parser";
import jsxA11y from "eslint-plugin-jsx-a11y";

// All jsx-a11y "recommended" rules, downgraded to warn. This bootstraps the
// plugin without failing CI on the ~50 pre-existing violations tracked in
// docs/design/ACCESSIBILITY_AUDIT.md — flip to the plugin's own severities once
// that backlog is cleared.
const jsxA11yWarnRules = Object.fromEntries(
  Object.entries(jsxA11y.flatConfigs.recommended.rules).map(([rule, config]) =>
    Array.isArray(config) ? [rule, ["warn", ...config.slice(1)]] : [rule, "warn"],
  ),
);

export default [
  { ignores: ["dist/**", "node_modules/**", "playwright-report/**", "test-results/**"] },
  {
    files: ["**/*.{ts,tsx}"],
    plugins: { "jsx-a11y": jsxA11y },
    languageOptions: {
      parser: tsParser,
      parserOptions: { ecmaFeatures: { jsx: true } },
    },
    rules: jsxA11yWarnRules,
  },
];

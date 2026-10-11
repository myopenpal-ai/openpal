import WritingToolsScreen from "./WritingToolsScreen";

/** The Correct page — see WritingToolsScreen.tsx for the shared
 * implementation. A distinct route from Translate (rather than one screen
 * with an in-page toggle) so the sidebar's Translate/Correct buttons are
 * plain navigation, and each page can be linked to, remembered, and
 * described on its own terms. */
export default function CorrectView() {
  return <WritingToolsScreen mode="correct" />;
}

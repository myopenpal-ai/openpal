import WritingToolsScreen from "./WritingToolsScreen";

/** The Translate page — see WritingToolsScreen.tsx for the shared
 * implementation. A distinct route from Correct (rather than one screen with
 * an in-page toggle) so the sidebar's Translate/Correct buttons are plain
 * navigation, and each page can be linked to, remembered, and described on
 * its own terms. */
export default function TranslateView() {
  return <WritingToolsScreen mode="translate" />;
}

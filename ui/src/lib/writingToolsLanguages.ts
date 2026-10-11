export interface WritingToolLanguage {
  id: string;
  name: string;
}

// Mirrors crates/core/src/writing_tools.rs's LANGUAGES exactly -- same ids,
// same names, same order. Keep both lists in sync by hand.
export const WRITING_TOOL_LANGUAGES: readonly WritingToolLanguage[] = [
  { id: "en", name: "English" },
  { id: "es", name: "Spanish" },
  { id: "fr", name: "French" },
  { id: "de", name: "German" },
  { id: "it", name: "Italian" },
  { id: "pt", name: "Portuguese" },
  { id: "nl", name: "Dutch" },
  { id: "sv", name: "Swedish" },
  { id: "no", name: "Norwegian" },
  { id: "da", name: "Danish" },
  { id: "fi", name: "Finnish" },
  { id: "pl", name: "Polish" },
  { id: "cs", name: "Czech" },
  { id: "ro", name: "Romanian" },
  { id: "hu", name: "Hungarian" },
  { id: "el", name: "Greek" },
  { id: "ru", name: "Russian" },
  { id: "uk", name: "Ukrainian" },
  { id: "tr", name: "Turkish" },
  { id: "ar", name: "Arabic" },
  { id: "he", name: "Hebrew" },
  { id: "hi", name: "Hindi" },
  { id: "vi", name: "Vietnamese" },
  { id: "th", name: "Thai" },
  { id: "id", name: "Indonesian" },
  { id: "zh", name: "Chinese (Simplified)" },
  { id: "ja", name: "Japanese" },
  { id: "ko", name: "Korean" },
];

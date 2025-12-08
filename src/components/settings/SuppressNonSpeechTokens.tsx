import React from "react";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface SuppressNonSpeechTokensProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const SuppressNonSpeechTokens: React.FC<SuppressNonSpeechTokensProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const suppressNonSpeechTokens = getSetting("suppress_non_speech_tokens") ?? true;

    const description = 
      "Controls whether Whisper suppresses non-speech tokens and audio markers during transcription. " +
      "When enabled (default), filters out tokens like [BLANK_AUDIO] and silence markers for cleaner output. " +
      "Note: Whisper's automatic punctuation is part of the model and cannot be fully disabled. " +
      "Disabling this may help with literal transcription of punctuation words, but results may vary.";

    return (
      <ToggleSwitch
        checked={suppressNonSpeechTokens}
        onChange={(enabled) => updateSetting("suppress_non_speech_tokens", enabled)}
        isUpdating={isUpdating("suppress_non_speech_tokens")}
        label="Suppress Non-Speech Tokens"
        description={description}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);

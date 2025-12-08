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
      "When enabled (default), filters out non-speech tokens like [BLANK_AUDIO] and silence markers. " +
      "Disable this if you want to say punctuation literally (e.g., saying 'comma' will transcribe as 'comma' " +
      "instead of inserting a comma symbol).";

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

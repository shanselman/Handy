import React from "react";
import { useTranslation } from "react-i18next";
import { type } from "@tauri-apps/plugin-os";
import { MicrophoneSelector } from "../MicrophoneSelector";
import { ChannelSelector } from "../ChannelSelector";
import { ShortcutInput } from "../ShortcutInput";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { OutputDeviceSelector } from "../OutputDeviceSelector";
import { ShortcutActivationSetting } from "../ShortcutActivation";
import { AudioFeedback } from "../AudioFeedback";
import { useSettings } from "../../../hooks/useSettings";
import { VolumeSlider } from "../VolumeSlider";
import { MuteWhileRecording } from "../MuteWhileRecording";
import { ModelSettingsCard } from "./ModelSettingsCard";
import { formatKeyCombination } from "../../../lib/utils/keyboard";

export const GeneralSettings: React.FC = () => {
  const { t } = useTranslation();
  const { audioFeedbackEnabled, getSetting } = useSettings();
  const osType = type();
  const isLinux = osType === "linux";
  const isWindows = osType === "windows";
  const annotationBinding = getSetting("bindings")?.transcribe_with_annotation;
  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.general.title")}>
        <ShortcutInput shortcutId="transcribe" grouped={true} />
        {isWindows && (
          <div
            role="group"
            aria-label={t(
              "settings.general.shortcut.bindings.transcribe_with_annotation.name",
              annotationBinding?.name || "",
            )}
          >
            <ShortcutInput
              shortcutId="transcribe_with_annotation"
              grouped={true}
            />
            {annotationBinding?.current_binding === "alt_left+space" && (
              <div className="mb-2 ml-6 border-l border-mid-gray/20">
                <div className="flex items-center justify-between min-h-10 py-2 pl-4 pr-12">
                  <h4 className="text-sm font-medium">
                    {t("screenAnnotation.nextPoint")}
                  </h4>
                  <kbd className="px-2 py-1 text-sm font-sans font-semibold bg-mid-gray/10 border border-mid-gray/40 rounded-md">
                    {formatKeyCombination("ctrl", osType)}
                  </kbd>
                </div>
              </div>
            )}
          </div>
        )}
        <ShortcutActivationSetting descriptionMode="tooltip" grouped={true} />
        {/* Cancel shortcut remains hidden on Linux because of dynamic shortcut instability. */}
        {!isLinux && <ShortcutInput shortcutId="cancel" grouped={true} />}
      </SettingsGroup>
      <ModelSettingsCard />
      <SettingsGroup title={t("settings.sound.title")}>
        <MicrophoneSelector descriptionMode="tooltip" grouped={true} />
        <ChannelSelector descriptionMode="tooltip" grouped={true} />
        <MuteWhileRecording descriptionMode="tooltip" grouped={true} />
        <AudioFeedback descriptionMode="tooltip" grouped={true} />
        <OutputDeviceSelector
          descriptionMode="tooltip"
          grouped={true}
          disabled={!audioFeedbackEnabled}
        />
        <VolumeSlider disabled={!audioFeedbackEnabled} />
      </SettingsGroup>
    </div>
  );
};

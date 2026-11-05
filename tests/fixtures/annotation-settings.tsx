import React from "react";
import { createRoot } from "react-dom/client";
import { mockIPC } from "@tauri-apps/api/mocks";
import type { AppSettings } from "../../src/bindings";
import "../../src/App.css";

const params = new URLSearchParams(location.search);
Object.assign(window, {
  __TAURI_OS_PLUGIN_INTERNALS__: {
    os_type: params.get("os") || "windows",
  },
});
mockIPC(
  (command) => {
    if (command === "get_app_settings") {
      return { app_language: params.get("language") || "en" };
    }
    throw new Error(`Unexpected test command: ${command}`);
  },
  { shouldMockEvents: true },
);

const { useSettingsStore } = await import("../../src/stores/settingsStore");
const settings: Partial<AppSettings> = {
  bindings: Object.fromEntries(
    [
      ["transcribe", "ctrl+space", "Transcribe Shortcut"],
      [
        "transcribe_with_annotation",
        params.get("binding") || "alt_left+space",
        "Transcribe + Annotate",
      ],
      ["cancel", "escape", "Cancel Shortcut"],
    ].map(([id, current_binding, name]) => [
      id,
      {
        id,
        current_binding,
        default_binding: current_binding,
        name,
        description: "",
      },
    ]),
  ),
  keyboard_implementation:
    params.get("implementation") === "handy_keys" ? "handy_keys" : "tauri",
};
useSettingsStore.setState({
  isLoading: false,
  getSetting: (key) => settings[key],
});
const { default: i18n } = await import("../../src/i18n");
await i18n.changeLanguage(params.get("language") || "en");
const { GeneralSettings } = await import(
  "../../src/components/settings/general/GeneralSettings"
);
createRoot(document.getElementById("root")!).render(<GeneralSettings />);
